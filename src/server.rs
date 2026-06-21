//! Authoritative game server over QUIC (quinn).
//!
//! The server uses a self-signed certificate persisted in its world directory,
//! so its fingerprint stays stable across restarts and clients' pinned TOFU
//! entries keep working. A brand-new world generates and saves a fresh pair on
//! first launch. The fingerprint is surfaced to the client so the TOFU flow
//! (see [`crate::client::net`]) can pin it. For singleplayer the client embeds
//! a server in-process and auto-trusts that fingerprint.

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use quinn::Endpoint;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use tokio::sync::mpsc;

use crate::block::BlockRegistry;
use crate::daylight;
use crate::entity::{Entities, Entity, EntityId, EntityKind, ITEM_SIZE, PLAYER_SIZE};
use crate::inventory::Inventory;
use crate::net::{fingerprint, read_msg, write_msg};
use crate::protocol::{ALPN, BlockId, ClientMessage, ServerMessage};
use crate::save::{SavedPlayer, WorldMeta, WorldStore};
use crate::world::{CHUNK_AREA, CHUNK_SIZE, ChunkCoord, TILE_SIZE, World};
use crate::worldgen::{Biome, WorldGen, spawn_point};

/// How often the server simulates non-player entities, in seconds.
const TICK_DT: f32 = 0.05;
/// Horizontal wander speed of a slime, in pixels/second.
const SLIME_SPEED: f32 = 30.0;
/// Leisurely wander speed of a chicken, in pixels/second.
const CHICKEN_WANDER_SPEED: f32 = 22.0;
/// Panicked run speed of a chicken fleeing a player, in pixels/second.
const CHICKEN_FLEE_SPEED: f32 = 70.0;
/// Seconds a chicken keeps bolting after being hit before settling down.
const CHICKEN_FLEE_TIME: f32 = 4.0;
/// Unhurried wander speed of a goat, in pixels/second.
const GOAT_SPEED: f32 = 18.0;
/// Shambling speed of a zombie, in pixels/second — noticeably slower than a
/// slime, so a player can outrun one but is in trouble if cornered.
const ZOMBIE_SPEED: f32 = 16.0;
/// How far (px) a zombie notices and chases a player. Reaches a little further
/// than a slime since zombies are the night's dedicated hunters.
const ZOMBIE_AGGRO: f32 = 200.0;
/// Maximum gap (px between AABBs) at which a zombie can land a hit.
const ZOMBIE_ATTACK_RANGE: f32 = 4.0;
/// Damage a zombie deals per hit — a heavy blow compared with a slime's nip.
const ZOMBIE_DAMAGE: i32 = 7;
/// Seconds a zombie waits between hits.
const ZOMBIE_ATTACK_INTERVAL: f32 = 1.2;
/// How often the server tries to spawn night zombies near players, in seconds.
const ZOMBIE_SPAWN_INTERVAL: f32 = 6.0;
/// Most live zombies allowed per connected player; spawning pauses at the cap.
const ZOMBIE_MAX_PER_PLAYER: usize = 5;
/// Nearest a freshly spawned zombie appears to its target player, in pixels —
/// far enough to be just off-screen so they don't pop in at point-blank range.
const ZOMBIE_SPAWN_MIN_DIST: f32 = 220.0;
/// Farthest a freshly spawned zombie appears from its target player, in pixels.
const ZOMBIE_SPAWN_MAX_DIST: f32 = 360.0;
/// How many spawn slots to scatter around the origin at world start. Each slot
/// spawns whatever creature its biome supports.
const SPAWN_SLOTS: i32 = 12;
/// Cell spacing between adjacent creature spawn slots.
const SPAWN_SPACING: i32 = 7;
/// Downward acceleration applied to simulated entities, in pixels/second².
const GRAVITY: f32 = 1400.0;
/// Terminal fall speed for simulated entities, in pixels/second.
const MAX_FALL: f32 = 900.0;
/// How far (px) a wandering creature strays from its home anchor before turning
/// back, keeping it loitering in a general area instead of marching off forever.
const WANDER_RANGE: f32 = 90.0;
/// Upward velocity (px/s) a ground creature uses to hop a single-block step in
/// its path. Tuned to clear one tile (16px) but not two, so creatures climb
/// gentle terrain without scaling walls.
const HOP_VELOCITY: f32 = -240.0;
/// Collision skin: keeps an entity's trailing edge from snapping into the next
/// cell when it is flush against a block boundary.
const EPS: f32 = 0.01;
/// How far (px) a slime notices and chases a player after dark.
const SLIME_AGGRO: f32 = 140.0;
/// Maximum gap (px between AABBs) at which a slime can land a hit.
const SLIME_ATTACK_RANGE: f32 = 4.0;
/// Damage a slime deals per bite.
const SLIME_DAMAGE: i32 = 3;
/// Seconds a slime waits between bites.
const SLIME_ATTACK_INTERVAL: f32 = 1.0;
/// Damage the player's melee swing deals.
const PLAYER_ATTACK_DAMAGE: i32 = 4;
/// Player melee reach (max gap, px, between attacker and target AABBs).
const PLAYER_ATTACK_REACH: f32 = 12.0;
/// Horizontal knockback speed (px/s) shoved onto whatever a hit lands on, away
/// from the attacker.
const KNOCKBACK_X: f32 = 180.0;
/// Upward knockback speed (px/s) — a small pop so a hit lifts the target a touch.
const KNOCKBACK_Y: f32 = 240.0;
/// How often the server broadcasts the current time of day, in seconds.
const TIME_BROADCAST_SECS: f32 = 2.0;
/// Upward velocity (px/s) given to a freshly mined block so it visibly pops out
/// of the ground instead of being collected instantly.
const ITEM_POP_VELOCITY: f32 = -120.0;
/// Seconds a dropped item must lie on the ground before it can be picked up.
/// Gives it time to pop clear of the player who mined it.
const ITEM_PICKUP_DELAY: f32 = 0.4;
/// Maximum gap (px between AABBs) at which a player collects a dropped item —
/// i.e. they must essentially be touching it.
const ITEM_PICKUP_REACH: f32 = 2.0;
/// Per-tick horizontal speed retained by a sliding item on the ground (drag).
const ITEM_GROUND_DRAG: f32 = 0.8;
/// How often the world is flushed to disk while running, in seconds.
const AUTOSAVE_SECS: f32 = 30.0;

/// Handle to a server running on its own thread + tokio runtime.
pub struct RunningServer {
    pub addr: SocketAddr,
    pub fingerprint: [u8; 32],
    // Keeping the endpoint alive keeps the server listening; dropping it (when
    // this handle is dropped) closes the server.
    _endpoint: Endpoint,
    /// Shared state, kept so the world can be flushed to disk when this handle
    /// is dropped (e.g. the player leaves a singleplayer session).
    shared: Arc<Shared>,
    /// Live LAN advertisement, if this server opted into discovery via
    /// [`RunningServer::advertise`]. Dropped (unregistering the service) when
    /// the server is.
    _discovery: Option<crate::discovery::LanAdvertiser>,
}

impl RunningServer {
    /// Announce this server on the local network under `name` so nearby clients
    /// can discover and join it without typing an address. Best-effort: a
    /// failure (no mDNS stack, firewall) is logged and otherwise ignored.
    ///
    /// Not called for the embedded singleplayer server, which binds loopback.
    pub fn advertise(&mut self, name: &str) {
        match crate::discovery::advertise(self.addr.port(), name, &self.fingerprint) {
            Ok(a) => self._discovery = Some(a),
            Err(e) => log::warn!("LAN discovery unavailable: {e:#}"),
        }
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        // Stop the autosave loop, then write a final, up-to-date save.
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.save();
    }
}

/// What the server thread reports back once the endpoint is listening (or the
/// error that stopped it): its address, certificate fingerprint, a handle to the
/// endpoint, and the shared state (so the caller can flush it on shutdown).
type Ready = Result<(SocketAddr, [u8; 32], Endpoint, Arc<Shared>)>;

/// One connected client, as seen by the server. The client's player avatar
/// lives in [`Shared::entities`] under the same id; this just holds the channel
/// used to push messages to it.
struct ClientHandle {
    tx: mpsc::UnboundedSender<ServerMessage>,
}

/// Server world: stored chunks, the generator that fills them on demand, a
/// block registry for solidity queries during entity collision, and the disk
/// store chunks are loaded from and saved to.
struct ServerWorld {
    world: World,
    generator: WorldGen,
    registry: BlockRegistry,
    store: WorldStore,
    /// Chunks modified since the last flush; only these are written to disk.
    dirty: HashSet<ChunkCoord>,
}

impl ServerWorld {
    /// Make sure chunk `(cx, cy)` is resident in memory: load it from disk if it
    /// was saved before, otherwise generate it fresh from the seed. Returns
    /// `true` only when the chunk was generated fresh this call (i.e. it had
    /// never existed before), so callers can react to brand-new terrain coming
    /// into being — e.g. seeding creatures into it.
    fn ensure(&mut self, cx: i32, cy: i32) -> bool {
        if self.world.has_chunk((cx, cy)) {
            return false;
        }
        let (chunk, fresh) = match self.store.load_chunk((cx, cy)) {
            Ok(Some(chunk)) => (chunk, false),
            Ok(None) => (self.generator.generate_chunk(cx, cy), true),
            Err(e) => {
                log::error!("failed to load chunk ({cx}, {cy}); regenerating: {e:#}");
                (self.generator.generate_chunk(cx, cy), true)
            }
        };
        self.world.insert_chunk((cx, cy), chunk);
        fresh
    }

    /// Write every dirty chunk to disk, clearing the dirty set on success.
    fn flush_chunks(&mut self) {
        for coord in self.dirty.drain() {
            if let Some(chunk) = self.world.get_chunk(coord)
                && let Err(e) = self.store.save_chunk(coord, chunk)
            {
                log::error!("failed to save chunk {coord:?}: {e:#}");
            }
        }
    }

    /// Block contents of chunk `(cx, cy)`, alongside whether this call was what
    /// first brought the chunk into existence (see [`ServerWorld::ensure`]).
    fn chunk_blocks(&mut self, cx: i32, cy: i32) -> (Vec<BlockId>, bool) {
        let fresh = self.ensure(cx, cy);
        let blocks = self
            .world
            .get_chunk((cx, cy))
            .map(|c| c.blocks.to_vec())
            .unwrap_or_else(|| vec![0; CHUNK_AREA]);
        (blocks, fresh)
    }

    /// Read the block at world cell `(x, y)`, generating its chunk on demand so
    /// the value is consistent wherever it is queried.
    fn get(&mut self, x: i32, y: i32) -> BlockId {
        self.ensure(x.div_euclid(CHUNK_SIZE), y.div_euclid(CHUNK_SIZE));
        self.world.get_block(x, y)
    }

    fn set(&mut self, x: i32, y: i32, b: BlockId) -> bool {
        let (cx, cy) = (x.div_euclid(CHUNK_SIZE), y.div_euclid(CHUNK_SIZE));
        self.ensure(cx, cy);
        let changed = self.world.set_block(x, y, b);
        if changed {
            self.dirty.insert((cx, cy));
        }
        changed
    }

    /// Surface (grass) row for a world column, used to place ground-walking
    /// entities when they first spawn.
    fn surface(&self, world_x: i32) -> i32 {
        self.generator.surface_height(world_x)
    }

    /// Which biome a world column belongs to, used to spawn the right creatures
    /// for the terrain a column sits in.
    fn biome(&self, world_x: i32) -> Biome {
        self.generator.biome_at(world_x)
    }

    /// Whether the block at world cell `(tx, ty)` collides with entities,
    /// generating the containing chunk on demand so collision is consistent
    /// wherever a slime wanders.
    fn solid(&mut self, tx: i32, ty: i32) -> bool {
        self.ensure(tx.div_euclid(CHUNK_SIZE), ty.div_euclid(CHUNK_SIZE));
        self.registry.is_solid(self.world.get_block(tx, ty))
    }
}

/// State shared across all connection tasks and the entity tick loop.
struct Shared {
    world: Mutex<ServerWorld>,
    clients: Mutex<HashMap<u32, ClientHandle>>,
    /// Every live entity (players and server-simulated creatures alike). A
    /// player avatar shares the id of its connection.
    entities: Mutex<Entities>,
    next_id: AtomicU32,
    spawn: (f32, f32),
    /// Reference instant the day/night clock counts from. Offset back in time on
    /// load so a resumed world keeps the time of day it was saved at.
    start: Instant,
    /// Saved state of every player who has joined, keyed by name. A player is
    /// moved out of here (into a live entity) while connected and folded back in
    /// on disconnect, so it survives both reconnects and restarts.
    saved_players: Mutex<HashMap<String, SavedPlayer>>,
    /// Slot inventory of every currently-connected player, keyed by entity id.
    /// Authoritative: placements consume from it and pickups add to it. Folded
    /// into [`SavedPlayer`] on disconnect so it persists.
    inventories: Mutex<HashMap<EntityId, Inventory>>,
    /// Set when the owning [`RunningServer`] is dropped, to stop the autosave
    /// loop.
    shutdown: AtomicBool,
}

impl Shared {
    fn alloc_id(&self) -> EntityId {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Current normalized time of day in `[0, 1)`.
    fn time_of_day(&self) -> f32 {
        daylight::time_of_day(self.start.elapsed().as_secs_f32())
    }

    /// Flush the whole world to disk: dirty chunks plus a fresh `world.dat`
    /// snapshot of the clock, entities, and players. Safe to call from any
    /// thread; logs (rather than propagates) IO errors so a failed save can
    /// never crash the server.
    fn save(&self) {
        let mut world = self.world.lock();
        world.flush_chunks();
        let seed = world.generator.seed();

        // Creatures save directly; players are gathered from both the saved set
        // and any currently-connected avatars (whose live state is freshest).
        let mut players = self.saved_players.lock().clone();
        let invs = self.inventories.lock().clone();
        let mut creatures = Vec::new();
        for e in self.entities.lock().values() {
            match &e.kind {
                EntityKind::Player { name } if !name.is_empty() => {
                    players.insert(
                        name.clone(),
                        SavedPlayer {
                            name: name.clone(),
                            x: e.x,
                            y: e.y,
                            health: e.health,
                            inventory: invs.get(&e.id).cloned().unwrap_or_default(),
                        },
                    );
                }
                EntityKind::Player { .. } => {} // unnamed: not yet identified
                _ => creatures.push(e.clone()),
            }
        }

        let meta = WorldMeta {
            seed,
            elapsed_secs: self.start.elapsed().as_secs_f32(),
            next_id: self.next_id.load(Ordering::SeqCst),
            spawn: self.spawn,
            entities: creatures,
            players: players.into_values().collect(),
        };
        if let Err(e) = world.store.save_meta(&meta) {
            log::error!("failed to save world: {e:#}");
        }
    }
}

impl Shared {
    fn broadcast_all(&self, msg: ServerMessage) {
        for h in self.clients.lock().values() {
            let _ = h.tx.send(msg.clone());
        }
    }

    fn broadcast_except(&self, except: u32, msg: ServerMessage) {
        for (id, h) in self.clients.lock().iter() {
            if *id != except {
                let _ = h.tx.send(msg.clone());
            }
        }
    }

    /// Send a message to a single client by id, if still connected.
    fn send_to(&self, id: u32, msg: ServerMessage) {
        if let Some(h) = self.clients.lock().get(&id) {
            let _ = h.tx.send(msg);
        }
    }

    /// Add one `block` to player `id`'s inventory, stacking into existing stacks
    /// first. Returns whether it fit (false only if the inventory is full).
    fn add_item(&self, id: EntityId, block: BlockId) -> bool {
        self.inventories.lock().entry(id).or_default().add(block, 1) == 0
    }

    /// Remove one item from hotbar/inventory `slot` of player `id`, returning the
    /// block taken (or `None` if the slot was empty). Used to pay for placement.
    fn take_from_slot(&self, id: EntityId, slot: usize) -> Option<BlockId> {
        self.inventories.lock().get_mut(&id)?.take_one(slot)
    }

    /// Rearrange player `id`'s inventory by moving slot `from` onto slot `to`.
    fn move_item(&self, id: EntityId, from: usize, to: usize) {
        if let Some(inv) = self.inventories.lock().get_mut(&id) {
            inv.move_stack(from, to);
        }
    }

    /// Push the authoritative inventory snapshot to its owner.
    fn send_inventory(&self, id: EntityId) {
        let slots = self
            .inventories
            .lock()
            .get(&id)
            .map(Inventory::to_slots)
            .unwrap_or_default();
        self.send_to(id, ServerMessage::Inventory { slots });
    }
}

/// Start a server on a background thread. `bind` of port 0 picks an ephemeral
/// port (used for the embedded singleplayer server). Returns once the endpoint
/// is listening, with its actual address and certificate fingerprint.
pub fn start_server(bind: SocketAddr, seed: i32, save_dir: PathBuf) -> Result<RunningServer> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Ready>();

    std::thread::Builder::new()
        .name("game-server".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.into()));
                    return;
                }
            };
            rt.block_on(async move {
                let shared = match setup(bind, seed, save_dir, &ready_tx).await {
                    Some(s) => s,
                    None => return,
                };
                // The endpoint is owned by RunningServer on the main thread; we
                // re-fetch via the shared accept loop below.
                accept_loop(shared).await;
            });
        })
        .context("spawning server thread")?;

    match ready_rx.recv().context("server failed to start")? {
        Ok((addr, fp, endpoint, shared)) => Ok(RunningServer {
            addr,
            fingerprint: fp,
            _endpoint: endpoint,
            shared,
            _discovery: None,
        }),
        Err(e) => Err(e),
    }
}

/// State needed by the accept loop.
struct AcceptCtx {
    endpoint: Endpoint,
    shared: Arc<Shared>,
}

async fn setup(
    bind: SocketAddr,
    seed: i32,
    save_dir: PathBuf,
    ready_tx: &std::sync::mpsc::Sender<Ready>,
) -> Option<AcceptCtx> {
    match build_endpoint(bind, seed, save_dir) {
        Ok((endpoint, fp, shared)) => {
            let addr = match endpoint.local_addr() {
                Ok(a) => a,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.into()));
                    return None;
                }
            };
            // Hand a clone of the endpoint to the caller (keeps it alive there),
            // keep our own for the accept loop.
            let _ = ready_tx.send(Ok((addr, fp, endpoint.clone(), shared.clone())));
            Some(AcceptCtx { endpoint, shared })
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            None
        }
    }
}

/// File names for the persisted TLS identity, stored alongside the world.
const CERT_FILE: &str = "cert.der";
const KEY_FILE: &str = "key.der";

/// Load the server's certificate and private key from `save_dir`, generating
/// and persisting a fresh self-signed pair on first run (or if the saved pair
/// is missing/unreadable). Persisting the pair keeps the certificate
/// fingerprint stable across restarts, so clients that pinned it via TOFU don't
/// see a (false) "certificate changed" alarm.
fn load_or_create_identity(
    save_dir: &Path,
) -> Result<(CertificateDer<'static>, PrivatePkcs8KeyDer<'static>)> {
    let cert_path = save_dir.join(CERT_FILE);
    let key_path = save_dir.join(KEY_FILE);

    // Reuse the saved pair when both files are present and readable.
    if let (Ok(cert), Ok(key)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
        return Ok((CertificateDer::from(cert), PrivatePkcs8KeyDer::from(key)));
    }

    // First launch (or unreadable): mint a fresh pair and persist it.
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .context("generating self-signed certificate")?;
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.signing_key.serialize_der();

    std::fs::create_dir_all(save_dir)
        .with_context(|| format!("creating {}", save_dir.display()))?;
    write_private_key(&key_path, &key_der)?;
    std::fs::write(&cert_path, &cert_der)
        .with_context(|| format!("writing {}", cert_path.display()))?;

    Ok((
        CertificateDer::from(cert_der),
        PrivatePkcs8KeyDer::from(key_der),
    ))
}

/// Write a private key to disk, restricting it to the owner on Unix.
fn write_private_key(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn build_endpoint(
    bind: SocketAddr,
    seed: i32,
    save_dir: PathBuf,
) -> Result<(Endpoint, [u8; 32], Arc<Shared>)> {
    // Self-signed certificate for "localhost", persisted so the fingerprint is
    // stable across restarts.
    let (cert_der, key_der) = load_or_create_identity(&save_dir)?;
    let fp = fingerprint(cert_der.as_ref());

    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der.into())
        .context("building rustls server config")?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let qsc = quinn::crypto::rustls::QuicServerConfig::try_from(crypto)
        .context("building QUIC server config")?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(qsc));

    let endpoint = Endpoint::server(server_config, bind).context("binding server endpoint")?;

    let store = WorldStore::new(save_dir);
    // Resume a previous save if one exists, otherwise create a fresh world.
    let saved = match store.load_meta() {
        Ok(meta) => meta,
        Err(e) => {
            log::error!("failed to load world save; starting fresh: {e:#}");
            None
        }
    };

    let generator = WorldGen::new(saved.as_ref().map(|m| m.seed).unwrap_or(seed));
    let spawn = saved
        .as_ref()
        .map(|m| m.spawn)
        .unwrap_or_else(|| spawn_point(&generator, 0));

    // Offset the clock so a loaded world resumes at the time of day it was saved.
    let start = match saved.as_ref() {
        Some(m) => Instant::now()
            .checked_sub(Duration::from_secs_f32(m.elapsed_secs))
            .unwrap_or_else(Instant::now),
        None => Instant::now(),
    };
    let next_id = saved.as_ref().map(|m| m.next_id.max(1)).unwrap_or(1);

    // Restore saved creatures into the live world; players go to the saved set.
    let mut entities = Entities::new();
    let mut saved_players = HashMap::new();
    if let Some(m) = &saved {
        for e in &m.entities {
            entities.insert(e.clone());
        }
        for p in &m.players {
            saved_players.insert(p.name.clone(), p.clone());
        }
    }

    let shared = Arc::new(Shared {
        world: Mutex::new(ServerWorld {
            world: World::new(),
            generator,
            registry: BlockRegistry::new(),
            store,
            dirty: HashSet::new(),
        }),
        clients: Mutex::new(HashMap::new()),
        entities: Mutex::new(entities),
        next_id: AtomicU32::new(next_id),
        spawn,
        start,
        saved_players: Mutex::new(saved_players),
        inventories: Mutex::new(HashMap::new()),
        shutdown: AtomicBool::new(false),
    });

    // Only seed fresh creatures for a brand-new world; a loaded one keeps its own.
    if saved.is_none() {
        spawn_creatures(&shared);
    }

    Ok((endpoint, fp, shared))
}

async fn accept_loop(ctx: AcceptCtx) {
    // Drive server-simulated entities on their own task.
    tokio::spawn(entity_tick_loop(ctx.shared.clone()));

    while let Some(incoming) = ctx.endpoint.accept().await {
        let shared = ctx.shared.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(incoming, shared).await {
                log::debug!("connection ended: {e:#}");
            }
        });
    }
}

/// Populate the world with biome-appropriate creatures scattered around the
/// origin: chickens roam the plains, while slimes and goats inhabit the
/// mountains. Each spawn slot looks up its column's biome and spawns whatever
/// belongs there.
fn spawn_creatures(shared: &Shared) {
    let world = shared.world.lock();
    let mut entities = shared.entities.lock();
    for i in 0..SPAWN_SLOTS {
        let cell_x = (i - SPAWN_SLOTS / 2) * SPAWN_SPACING;
        // Alternate hostile/placid creatures in mountains; `i` parity keeps the
        // original mix around the origin.
        let kind = creature_for_biome(world.biome(cell_x), i % 2 == 0);
        let (_, h) = kind.size();
        let surface = world.surface(cell_x);
        let id = shared.alloc_id();
        let x = cell_x as f32 * TILE_SIZE;
        let y = surface as f32 * TILE_SIZE - h;
        entities.insert(Entity::new(id, kind, x, y));
    }
}

/// Which creature a column's biome supports. `hostile_slot` selects between the
/// two mountain dwellers (slime when true, goat when false) so callers can mix
/// them; plains always yield a chicken.
fn creature_for_biome(biome: Biome, hostile_slot: bool) -> EntityKind {
    match biome {
        // Plains and forest: peaceful chickens beneath the trees.
        Biome::Plains | Biome::Forest => EntityKind::Chicken,
        // Mountains: hostile slimes interspersed with placid goats.
        Biome::Mountains => {
            if hostile_slot {
                EntityKind::Slime
            } else {
                EntityKind::Goat
            }
        }
    }
}

/// Deterministic pseudo-random value for chunk `(cx, cy)` mixed with `salt`,
/// derived from the world seed so the same chunk always makes the same spawn
/// decision regardless of who explores it or when.
fn chunk_hash(seed: i32, cx: i32, cy: i32, salt: u32) -> u32 {
    let mut h = (seed as u32)
        .wrapping_mul(374_761_393)
        .wrapping_add((cx as u32).wrapping_mul(668_265_263))
        .wrapping_add((cy as u32).wrapping_mul(2_246_822_519));
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h = h.wrapping_add(salt.wrapping_mul(0x9E37_79B9));
    h ^ (h >> 16)
}

/// Percent chance that a freshly generated surface chunk seeds creatures.
const CHUNK_SPAWN_CHANCE: u32 = 40;
/// Most creatures a single chunk can seed at once.
const CHUNK_SPAWN_MAX: u32 = 3;

/// Possibly seed biome-appropriate creatures into a chunk that has just come
/// into existence, broadcasting any spawns to connected clients. Only surface
/// chunks (those the terrain's grass line passes through) are eligible, and the
/// decision is deterministic per chunk via [`chunk_hash`], so exploring the same
/// terrain never double-spawns. Mirrors [`spawn_creatures`] for placement.
fn maybe_spawn_in_chunk(shared: &Shared, cx: i32, cy: i32) {
    let world = shared.world.lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 0) % 100 >= CHUNK_SPAWN_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let count = 1 + chunk_hash(seed, cx, cy, 1) % CHUNK_SPAWN_MAX;

    let mut spawned = Vec::new();
    {
        let mut entities = shared.entities.lock();
        for n in 0..count {
            // Scatter spawns across the chunk's columns.
            let lx = chunk_hash(seed, cx, cy, 2 + n) % CHUNK_SIZE as u32;
            let cell_x = base_x + lx as i32;
            let surface = world.surface(cell_x);
            // Only spawn where this chunk actually contains the ground surface,
            // so creatures never appear buried in stone or floating in the sky.
            if surface < chunk_top || surface >= chunk_bottom {
                continue;
            }
            let kind = creature_for_biome(world.biome(cell_x), n % 2 == 0);
            let (_, h) = kind.size();
            let id = shared.alloc_id();
            let x = cell_x as f32 * TILE_SIZE;
            let y = surface as f32 * TILE_SIZE - h;
            let entity = Entity::new(id, kind, x, y);
            entities.insert(entity.clone());
            spawned.push(entity);
        }
    }
    drop(world);

    for entity in spawned {
        shared.broadcast_all(ServerMessage::EntitySpawn { entity });
    }
}

/// Advance a small xorshift RNG state and return the new value. Used to scatter
/// night-zombie spawns; determinism isn't important here, only cheap variety.
fn next_rng(state: &mut u32) {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
}

/// At night, try to spawn a zombie just off-screen from a random player, up to a
/// per-player cap. Zombies appear in any biome — they belong to the dark, not to
/// the terrain — so unlike the biome critters this ignores the column's biome.
/// Does nothing during the day or when no players are connected.
fn maybe_spawn_zombies(shared: &Shared, rng: &mut u32) {
    if !daylight::is_night(shared.time_of_day()) {
        return;
    }

    // world then entities, matching the lock order used elsewhere.
    let world = shared.world.lock();
    let mut entities = shared.entities.lock();

    let players: Vec<(f32, f32)> = entities
        .values()
        .filter(|e| e.kind.is_player())
        .map(|e| (e.x, e.y))
        .collect();
    if players.is_empty() {
        return;
    }
    let zombies = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Zombie))
        .count();
    if zombies >= players.len() * ZOMBIE_MAX_PER_PLAYER {
        return;
    }

    // Pick a player, a side, and a distance to drop the zombie at.
    next_rng(rng);
    let (px, py) = players[(*rng as usize) % players.len()];
    next_rng(rng);
    let side = if *rng & 1 == 0 { -1.0 } else { 1.0 };
    next_rng(rng);
    let span = (ZOMBIE_SPAWN_MAX_DIST - ZOMBIE_SPAWN_MIN_DIST) as u32;
    let dist = ZOMBIE_SPAWN_MIN_DIST + (*rng % span.max(1)) as f32;

    let kind = EntityKind::Zombie;
    let (_, h) = kind.size();
    let cell_x = ((px + side * dist) / TILE_SIZE).floor() as i32;
    let surface = world.surface(cell_x);
    let x = cell_x as f32 * TILE_SIZE;
    let y = surface as f32 * TILE_SIZE - h;
    // Skip spawns that land jarringly far above the player (e.g. across a deep
    // ravine), so a zombie never materializes hanging in open sky beside them.
    if (y - py).abs() > 400.0 {
        return;
    }

    let id = shared.alloc_id();
    let zombie = Entity::new(id, kind, x, y);
    entities.insert(zombie.clone());
    drop(entities);
    drop(world);
    shared.broadcast_all(ServerMessage::EntitySpawn { entity: zombie });
}

/// Spawn a dropped-block item at the center of cell `(cell_x, cell_y)`, popping
/// it upward so it clears the player who mined it, and announce it to everyone.
fn spawn_drop(shared: &Shared, cell_x: i32, cell_y: i32, block: BlockId) {
    let (iw, ih) = ITEM_SIZE;
    let id = shared.alloc_id();
    let x = cell_x as f32 * TILE_SIZE + (TILE_SIZE - iw) * 0.5;
    let y = cell_y as f32 * TILE_SIZE + (TILE_SIZE - ih) * 0.5;
    let mut item = Entity::new(id, EntityKind::DroppedItem { block }, x, y);
    item.vy = ITEM_POP_VELOCITY;
    item.attack_cd = ITEM_PICKUP_DELAY; // reused as the pickup-delay timer
    shared.entities.lock().insert(item.clone());
    shared.broadcast_all(ServerMessage::EntitySpawn { entity: item });
}

/// Periodically simulates non-player entities, applies survival rules, and
/// broadcasts the results (motion, health, time of day, respawns).
async fn entity_tick_loop(shared: Arc<Shared>) {
    let mut interval = tokio::time::interval(Duration::from_secs_f32(TICK_DT));
    let mut since_time_bcast = 0.0f32;
    let mut since_save = 0.0f32;
    let mut since_zombie_spawn = 0.0f32;
    // Evolving RNG state for scattering night-zombie spawns.
    let mut zombie_rng = 0x9E37_79B9u32;
    loop {
        interval.tick().await;

        // Stop once the session is shutting down; the final save is written by
        // RunningServer::drop.
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }

        let Step {
            broadcasts,
            respawns,
            mut pickups,
        } = step_entities(&shared);
        for msg in broadcasts {
            shared.broadcast_all(msg);
        }
        for (id, x, y) in respawns {
            shared.send_to(id, ServerMessage::Respawn { x, y });
        }
        // Items were credited during the step; push each collector a fresh
        // inventory snapshot (deduplicating repeat collectors).
        pickups.sort_unstable();
        pickups.dedup();
        for pid in pickups {
            shared.send_inventory(pid);
        }

        // After dark, periodically conjure zombies near the players.
        since_zombie_spawn += TICK_DT;
        if since_zombie_spawn >= ZOMBIE_SPAWN_INTERVAL {
            since_zombie_spawn = 0.0;
            maybe_spawn_zombies(&shared, &mut zombie_rng);
        }

        // Keep every client's day/night clock in sync.
        since_time_bcast += TICK_DT;
        if since_time_bcast >= TIME_BROADCAST_SECS {
            since_time_bcast = 0.0;
            shared.broadcast_all(ServerMessage::TimeOfDay {
                t: shared.time_of_day(),
            });
        }

        // Periodically persist the world so progress survives a crash.
        since_save += TICK_DT;
        if since_save >= AUTOSAVE_SECS {
            since_save = 0.0;
            shared.save();
        }
    }
}

/// Outcome of one simulation tick: messages to broadcast to everyone, and
/// per-player respawn targets to send to their owners.
struct Step {
    broadcasts: Vec<ServerMessage>,
    respawns: Vec<(EntityId, f32, f32)>,
    /// Players who collected an item this tick and so need a fresh inventory
    /// snapshot (sent after the entity locks are released). May contain repeats.
    pickups: Vec<EntityId>,
}

/// Advance every server-simulated entity by one tick. Players are skipped for
/// motion (they are authoritative on their owning client) but can still be
/// targeted and bitten by slimes at night.
///
/// Locks `world` then `entities` for the whole step, matching the order used by
/// [`spawn_slimes`] so the two can never deadlock.
fn step_entities(shared: &Shared) -> Step {
    let night = daylight::is_night(shared.time_of_day());
    let mut world = shared.world.lock();
    let mut entities = shared.entities.lock();

    // Snapshot player positions up front so slime targeting doesn't fight the
    // borrow checker over a second mutable handle into the map.
    let players: Vec<(EntityId, f32, f32)> = entities
        .values()
        .filter(|e| e.kind.is_player())
        .map(|e| (e.id, e.x, e.y))
        .collect();

    let slime_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Slime))
        .map(|e| e.id)
        .collect();
    let chicken_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Chicken))
        .map(|e| e.id)
        .collect();
    let goat_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Goat))
        .map(|e| e.id)
        .collect();
    let zombie_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Zombie))
        .map(|e| e.id)
        .collect();
    let item_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| e.kind.is_item())
        .map(|e| e.id)
        .collect();

    let mut broadcasts = Vec::new();
    let mut pickups: Vec<EntityId> = Vec::new();
    // Players a creature hit this tick; applied after the movement loop so we
    // never hold two mutable entity borrows at once. Each entry is the bitten
    // player, the knockback `(vx, vy)` to shove it away from the attacker, and
    // the damage dealt (slimes nip, zombies hit hard).
    let mut bites: Vec<(EntityId, (f32, f32), i32)> = Vec::new();

    for id in slime_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);

        // At night, lock onto the nearest player within aggro range.
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = if night {
            nearest_player(&players, scx, scy, SLIME_AGGRO)
        } else {
            None
        };
        let chasing = target.is_some();

        // Heading: toward the target when chasing, else wander within its home
        // range (turning back once it strays too far).
        let dir = match target {
            Some((_, px, _)) if px + PLAYER_SIZE.0 * 0.5 < scx => -1.0,
            Some(_) => 1.0,
            None => wander_dir(scx, e.vx, home),
        };

        // A chasing slime commits to the chase (over ledges and cliffs); a
        // patrolling one negotiates one-block steps and turns back at walls and
        // deep drops.
        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            SLIME_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });

        // Bite the target if it is in reach and the slime is off cooldown.
        if let Some((pid, px, py)) = target {
            if e.attack_cd <= 0.0
                && aabb_gap(m.x, m.y, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1)
                    <= SLIME_ATTACK_RANGE
            {
                e.attack_cd = SLIME_ATTACK_INTERVAL;
                let dir = if px + PLAYER_SIZE.0 * 0.5 >= m.x + w * 0.5 {
                    1.0
                } else {
                    -1.0
                };
                bites.push((pid, (dir * KNOCKBACK_X, -KNOCKBACK_Y), SLIME_DAMAGE));
            }
        }
    }

    // Chickens: peck around peacefully, but bolt away from the nearest player
    // for a few seconds after being hit (the flee timer is set in apply_damage).
    for id in chicken_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.flee = (e.flee - TICK_DT).max(0.0);
        let fleeing = e.flee > 0.0;
        let scx = e.x + w * 0.5;
        let home = *e.home_x.get_or_insert(e.x);

        let dir = if fleeing {
            // Run away from the nearest player, but if an unclimbable wall blocks
            // that escape, veer the other way instead of running into it.
            let away = match nearest_player(&players, scx, e.y + h * 0.5, f32::INFINITY) {
                Some((_, px, _)) if px + PLAYER_SIZE.0 * 0.5 < scx => 1.0,
                Some(_) => -1.0,
                None if e.vx < 0.0 => -1.0,
                None => 1.0,
            };
            if blocked_ahead(&mut world, e.x, e.y, w, h, away) {
                -away
            } else {
                away
            }
        } else {
            wander_dir(scx, e.vx, home)
        };
        let speed = if fleeing {
            CHICKEN_FLEE_SPEED
        } else {
            CHICKEN_WANDER_SPEED
        };

        let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, speed, fleeing);
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }

    // Goats: calm grazers that simply amble around their home patch of mountain,
    // negotiating one-block steps and turning back at walls and ledges.
    for id in goat_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        let scx = e.x + w * 0.5;
        let home = *e.home_x.get_or_insert(e.x);
        let dir = wander_dir(scx, e.vx, home);
        let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, dir, GOAT_SPEED, false);
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });
    }

    // Zombies: slow, relentless night hunters that hit hard. When day breaks
    // they crumble where they stand, playing a death animation before despawning.
    // Ids whose death animation finished this tick and must be removed below.
    let mut zombie_despawns: Vec<EntityId> = Vec::new();
    for id in zombie_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // Already crumbling: hold still (gravity still settles it onto ground),
        // run out the death timer, then mark it for removal.
        if e.dying > 0.0 {
            e.dying -= TICK_DT;
            let m = step_ground(&mut world, (e.x, e.y, w, h), e.vy, 0.0, 0.0, false);
            e.x = m.x;
            e.y = m.y;
            e.vx = 0.0;
            e.vy = m.vy;
            broadcasts.push(ServerMessage::EntityMoved {
                id,
                x: m.x,
                y: m.y,
                vx: 0.0,
                vy: m.vy,
            });
            if e.dying <= 0.0 {
                zombie_despawns.push(id);
            }
            continue;
        }

        // Daybreak: begin dying. Tell every client to play the crumble.
        if !night {
            e.dying = crate::entity::ZOMBIE_DEATH_TIME;
            e.vx = 0.0;
            broadcasts.push(ServerMessage::EntityDying { id });
            continue;
        }

        // Night: shamble after the nearest player within aggro range, otherwise
        // wander its home patch like the other ground creatures.
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_player(&players, scx, scy, ZOMBIE_AGGRO);
        let chasing = target.is_some();
        let dir = match target {
            Some((_, px, _)) if px + PLAYER_SIZE.0 * 0.5 < scx => -1.0,
            Some(_) => 1.0,
            None => wander_dir(scx, e.vx, home),
        };

        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            ZOMBIE_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: m.vx,
            vy: m.vy,
        });

        if let Some((pid, px, py)) = target {
            if e.attack_cd <= 0.0
                && aabb_gap(m.x, m.y, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1)
                    <= ZOMBIE_ATTACK_RANGE
            {
                e.attack_cd = ZOMBIE_ATTACK_INTERVAL;
                let dir = if px + PLAYER_SIZE.0 * 0.5 >= m.x + w * 0.5 {
                    1.0
                } else {
                    -1.0
                };
                bites.push((pid, (dir * KNOCKBACK_X, -KNOCKBACK_Y), ZOMBIE_DAMAGE));
            }
        }
    }
    for id in zombie_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Dropped items: fall under gravity, then get collected by any player that
    // is touching them once their pickup delay has elapsed.
    for id in item_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        let EntityKind::DroppedItem { block } = e.kind else {
            continue;
        };
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0); // reused as pickup delay

        let mut vx = e.vx;
        let mut vy = (e.vy + GRAVITY * TICK_DT).min(MAX_FALL);
        let (x, hit_wall) = move_x(&mut world, e.x, e.y, w, h, vx * TICK_DT);
        if hit_wall {
            vx = 0.0;
        }
        let (y, on_ground) = move_y(&mut world, x, e.y, w, h, vy * TICK_DT);
        if on_ground {
            vy = 0.0;
            vx *= ITEM_GROUND_DRAG;
            if vx.abs() < 1.0 {
                vx = 0.0;
            }
        }
        e.x = x;
        e.y = y;
        e.vx = vx;
        e.vy = vy;

        // Collect into the first touching player with room (delay permitting).
        let collector = if e.attack_cd <= 0.0 {
            players.iter().find(|&&(pid, px, py)| {
                aabb_gap(x, y, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1) <= ITEM_PICKUP_REACH
                    && shared.add_item(pid, block)
            })
        } else {
            None
        };
        if let Some(&(pid, _, _)) = collector {
            entities.remove(id);
            broadcasts.push(ServerMessage::EntityDespawn { id });
            pickups.push(pid);
        } else {
            broadcasts.push(ServerMessage::EntityMoved { id, x, y, vx, vy });
        }
    }

    let mut respawns = Vec::new();
    for (pid, kb, damage) in bites {
        let (msgs, respawn) = apply_damage(&mut entities, pid, damage, kb, shared.spawn);
        broadcasts.extend(msgs);
        if let Some(r) = respawn {
            respawns.push(r);
        }
    }

    Step {
        broadcasts,
        respawns,
        pickups,
    }
}

/// Nearest player (returned as its top-left `(id, x, y)`) whose center is within
/// `range` of `(x, y)`, or `None` if none are close enough.
fn nearest_player(
    players: &[(EntityId, f32, f32)],
    x: f32,
    y: f32,
    range: f32,
) -> Option<(EntityId, f32, f32)> {
    let mut best: Option<(EntityId, f32, f32, f32)> = None;
    for &(pid, px, py) in players {
        let dx = (px + PLAYER_SIZE.0 * 0.5) - x;
        let dy = (py + PLAYER_SIZE.1 * 0.5) - y;
        let d2 = dx * dx + dy * dy;
        if d2 <= range * range && best.is_none_or(|(_, _, _, bd)| d2 < bd) {
            best = Some((pid, px, py, d2));
        }
    }
    best.map(|(pid, px, py, _)| (pid, px, py))
}

/// Smallest gap (px) between two AABBs; `0.0` when they overlap.
fn aabb_gap(ax: f32, ay: f32, aw: f32, ah: f32, bx: f32, by: f32, bw: f32, bh: f32) -> f32 {
    let gx = (bx - (ax + aw)).max(ax - (bx + bw)).max(0.0);
    let gy = (by - (ay + ah)).max(ay - (by + bh)).max(0.0);
    gx.max(gy)
}

/// Apply `amount` damage to entity `id` (caller already holds the entities
/// lock). Returns the messages to broadcast and, if a *player* died, the
/// `(id, x, y)` respawn target to send to its owner. A non-player that dies is
/// removed from the world.
fn apply_damage(
    entities: &mut Entities,
    id: EntityId,
    amount: i32,
    knockback: (f32, f32),
    spawn: (f32, f32),
) -> (Vec<ServerMessage>, Option<(EntityId, f32, f32)>) {
    let Some(e) = entities.get_mut(id) else {
        return (Vec::new(), None);
    };
    e.health = (e.health - amount).max(0);

    // Knockback: shove server-simulated creatures directly (their motion is
    // authoritative here, so the next tick carries them off). Player avatars are
    // client-authoritative, so we can't move them from here — the owning client
    // applies the same velocity when it sees the EntityHit below.
    if !e.kind.is_player() {
        e.vx += knockback.0;
        e.vy += knockback.1;
    }

    // Getting hit sends a chicken into a panicked sprint away from players.
    if matches!(e.kind, EntityKind::Chicken) {
        e.flee = CHICKEN_FLEE_TIME;
    }

    // Every hit flashes the target red on all clients and carries the knockback.
    let mut msgs = vec![ServerMessage::EntityHit {
        id,
        vx: knockback.0,
        vy: knockback.1,
    }];

    if e.health > 0 {
        msgs.push(ServerMessage::EntityHealth {
            id,
            health: e.health,
            max_health: e.max_health,
        });
        return (msgs, None);
    }

    if e.kind.is_player() {
        // Death = respawn at full health back at spawn.
        e.health = e.max_health;
        e.x = spawn.0;
        e.y = spawn.1;
        let health = e.health;
        let max_health = e.max_health;
        msgs.push(ServerMessage::EntityHealth {
            id,
            health,
            max_health,
        });
        (msgs, Some((id, spawn.0, spawn.1)))
    } else {
        entities.remove(id);
        msgs.push(ServerMessage::EntityDespawn { id });
        (msgs, None)
    }
}

/// Move an AABB horizontally by `dx`, stopping at the first solid column.
/// Returns the resolved x and whether a wall was hit.
fn move_x(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, dx: f32) -> (f32, bool) {
    if dx == 0.0 {
        return (x, false);
    }
    let new_x = x + dx;
    let y0 = (y / TILE_SIZE).floor() as i32;
    let y1 = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    if dx > 0.0 {
        let tx = ((new_x + w - EPS) / TILE_SIZE).floor() as i32;
        if (y0..=y1).any(|ty| world.solid(tx, ty)) {
            return (tx as f32 * TILE_SIZE - w, true);
        }
    } else {
        let tx = (new_x / TILE_SIZE).floor() as i32;
        if (y0..=y1).any(|ty| world.solid(tx, ty)) {
            return ((tx + 1) as f32 * TILE_SIZE, true);
        }
    }
    (new_x, false)
}

/// Move an AABB vertically by `dy`, stopping at the first solid row. Returns the
/// resolved y and whether the entity is now resting on the ground.
fn move_y(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, dy: f32) -> (f32, bool) {
    if dy == 0.0 {
        return (y, false);
    }
    let new_y = y + dy;
    let x0 = (x / TILE_SIZE).floor() as i32;
    let x1 = ((x + w - EPS) / TILE_SIZE).floor() as i32;
    if dy > 0.0 {
        let ty = ((new_y + h - EPS) / TILE_SIZE).floor() as i32;
        if (x0..=x1).any(|tx| world.solid(tx, ty)) {
            return (ty as f32 * TILE_SIZE - h, true);
        }
    } else {
        let ty = (new_y / TILE_SIZE).floor() as i32;
        if (x0..=x1).any(|tx| world.solid(tx, ty)) {
            return ((ty + 1) as f32 * TILE_SIZE, true);
        }
    }
    (new_y, false)
}

/// Resolved motion of a ground-walking creature after one [`step_ground`] tick.
struct GroundMotion {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
}

/// Advance a ground-walking creature (slime, chicken) one tick.
///
/// It moves horizontally in direction `dir` (sign of intent) at `speed`, falls
/// under gravity, and negotiates one-block height changes so it roams across
/// uneven ground instead of being trapped on a single level: it hops a
/// single-block step in its path and walks down a single-block drop. A
/// `committed` creature (chasing or fleeing) bulls through — never reversing and
/// following the player off cliffs — while an uncommitted (wandering) one turns
/// around at walls it can't hop and at drops deeper than one block. The returned
/// `vx` carries the post-turn heading so patrol direction persists across ticks.
fn step_ground(
    world: &mut ServerWorld,
    aabb: (f32, f32, f32, f32),
    vy_in: f32,
    dir: f32,
    speed: f32,
    committed: bool,
) -> GroundMotion {
    let (x, y, w, h) = aabb;
    let grounded_before = grounded(world, x, y, w, h);
    let mut vx = dir * speed;
    let mut vy = (vy_in + GRAVITY * TICK_DT).min(MAX_FALL);

    // Horizontal first, then vertical — each axis resolved independently.
    let (nx, hit_wall) = move_x(world, x, y, w, h, vx * TICK_DT);

    // Hop a single-block step we ran into, if there's headroom to clear it.
    if grounded_before && hit_wall && can_step_up(world, nx, y, w, h, vx) {
        vy = HOP_VELOCITY;
    }

    let (ny, on_ground) = move_y(world, nx, y, w, h, vy * TICK_DT);
    if on_ground {
        vy = 0.0;
    }

    // Wandering creatures reverse at walls they couldn't hop (vy not launched
    // upward) and at drops too deep to step down. Committed ones never turn.
    if !committed
        && on_ground
        && ((hit_wall && vy >= 0.0) || drop_ahead(world, nx, ny, w, h, vx) >= 2)
    {
        vx = -vx;
    }

    GroundMotion {
        x: nx,
        y: ny,
        vx,
        vy,
    }
}

/// Heading (`-1.0`/`1.0`) for a wandering creature whose center is at `center_x`:
/// keep going the way it was, but turn back toward `home_x` once it strays past
/// [`WANDER_RANGE`], so it loiters in one area instead of marching off.
fn wander_dir(center_x: f32, vx: f32, home_x: f32) -> f32 {
    if center_x > home_x + WANDER_RANGE {
        -1.0
    } else if center_x < home_x - WANDER_RANGE {
        1.0
    } else if vx < 0.0 {
        -1.0
    } else {
        1.0
    }
}

/// Whether an unclimbable wall blocks a creature trying to move in direction
/// `dir`: a solid cell ahead at body height that a single hop can't clear. Lets
/// a fleeing creature veer around walls instead of running headlong into them.
fn blocked_ahead(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, dir: f32) -> bool {
    let ahead = if dir > 0.0 { x + w + EPS } else { x - EPS };
    let tx = (ahead / TILE_SIZE).floor() as i32;
    let y0 = (y / TILE_SIZE).floor() as i32;
    let y1 = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    let wall = (y0..=y1).any(|ty| world.solid(tx, ty));
    wall && !can_step_up(world, x, y, w, h, dir)
}

/// Whether an entity's AABB is resting on solid ground (a solid cell directly
/// beneath its feet).
fn grounded(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32) -> bool {
    let ty = ((y + h + EPS) / TILE_SIZE).floor() as i32;
    let x0 = (x / TILE_SIZE).floor() as i32;
    let x1 = ((x + w - EPS) / TILE_SIZE).floor() as i32;
    (x0..=x1).any(|tx| world.solid(tx, ty))
}

/// Whether a grounded creature heading `vx` can clear the block directly in its
/// path with a single hop: the cell ahead at foot level is solid, but the two
/// cells above it are clear, leaving room to rise and land one block up.
fn can_step_up(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, vx: f32) -> bool {
    let ahead = if vx > 0.0 { x + w + EPS } else { x - EPS };
    let tx = (ahead / TILE_SIZE).floor() as i32;
    let foot = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    world.solid(tx, foot) && !world.solid(tx, foot - 1) && !world.solid(tx, foot - 2)
}

/// How far the ground drops just ahead of a grounded creature, in tiles: `0` is
/// level ground (or a wall) ahead, `1` is a single-block step down, and `2`
/// means a drop of two or more blocks (a cliff). Capped at `3` so a bottomless
/// gap doesn't scan forever.
fn drop_ahead(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, vx: f32) -> i32 {
    let ahead = if vx > 0.0 { x + w + EPS } else { x - EPS };
    let tx = (ahead / TILE_SIZE).floor() as i32;
    let ground = ((y + h + EPS) / TILE_SIZE).floor() as i32;
    let mut d = 0;
    while d < 3 && !world.solid(tx, ground + d) {
        d += 1;
    }
    d
}

async fn handle_connection(incoming: quinn::Incoming, shared: Arc<Shared>) -> Result<()> {
    let connection = incoming.await.context("accepting connection")?;
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .context("accepting bidirectional stream")?;

    let id = shared.alloc_id();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    // Writer task: drains outbound messages to the send stream.
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write_msg(&mut send, &msg).await.is_err() {
                break;
            }
        }
        let _ = send.finish();
    });

    // Register the player and welcome them.
    let (sx, sy) = shared.spawn;
    let _ = tx.send(ServerMessage::Welcome {
        entity_id: id,
        spawn_x: sx,
        spawn_y: sy,
    });
    // Tell the owner its starting health (its own avatar is never mirrored via
    // EntitySpawn) and the current time of day.
    let _ = tx.send(ServerMessage::EntityHealth {
        id,
        health: crate::entity::PLAYER_MAX_HEALTH,
        max_health: crate::entity::PLAYER_MAX_HEALTH,
    });
    let _ = tx.send(ServerMessage::TimeOfDay {
        t: shared.time_of_day(),
    });

    // Send the newcomer a snapshot of every existing entity, then register and
    // announce their own player entity to everyone else.
    let player = Entity::new(
        id,
        EntityKind::Player {
            name: String::new(),
        },
        sx,
        sy,
    );
    {
        let mut entities = shared.entities.lock();
        for e in entities.values() {
            let _ = tx.send(ServerMessage::EntitySpawn { entity: e.clone() });
        }
        entities.insert(player.clone());
    }
    shared
        .clients
        .lock()
        .insert(id, ClientHandle { tx: tx.clone() });
    shared.broadcast_except(id, ServerMessage::EntitySpawn { entity: player });
    // Start with an empty inventory; a returning player gets theirs restored on
    // Hello. Either way the owner is told its contents.
    shared.inventories.lock().insert(id, Inventory::new());
    shared.send_inventory(id);
    log::info!("player {id} connected");

    // Reader loop.
    let read_result: Result<()> = async {
        loop {
            let msg: ClientMessage = read_msg(&mut recv).await?;
            match msg {
                ClientMessage::Hello { name } => {
                    log::info!("player {id} is '{name}'");
                    // If this name has saved state, move them back into it.
                    let restored = shared.saved_players.lock().remove(&name);
                    if let Some(e) = shared.entities.lock().get_mut(id) {
                        e.kind = EntityKind::Player { name };
                        if let Some(sp) = &restored {
                            e.x = sp.x;
                            e.y = sp.y;
                            e.health = sp.health;
                        }
                    }
                    if let Some(sp) = restored {
                        // Restore their saved inventory and push it to them.
                        shared.inventories.lock().insert(id, sp.inventory.clone());
                        shared.send_inventory(id);
                        // Teleport the owner's avatar and resync its health.
                        let _ = tx.send(ServerMessage::Respawn { x: sp.x, y: sp.y });
                        shared.broadcast_all(ServerMessage::EntityHealth {
                            id,
                            health: sp.health,
                            max_health: crate::entity::PLAYER_MAX_HEALTH,
                        });
                        shared.broadcast_except(
                            id,
                            ServerMessage::EntityMoved {
                                id,
                                x: sp.x,
                                y: sp.y,
                                vx: 0.0,
                                vy: 0.0,
                            },
                        );
                    }
                }
                ClientMessage::RequestChunk { cx, cy } => {
                    let (blocks, fresh) = shared.world.lock().chunk_blocks(cx, cy);
                    let _ = tx.send(ServerMessage::Chunk { cx, cy, blocks });
                    // A chunk coming into existence for the first time has a
                    // medium chance to seed creatures into its terrain.
                    if fresh {
                        maybe_spawn_in_chunk(&shared, cx, cy);
                    }
                }
                ClientMessage::SetBlock { x, y, block: _ } => {
                    // Breaking: clear the cell and drop its block on the ground
                    // for the player to walk over and collect.
                    let mined = {
                        let mut w = shared.world.lock();
                        let prev = w.get(x, y);
                        if prev != crate::block::AIR && w.set(x, y, crate::block::AIR) {
                            Some(prev)
                        } else {
                            None
                        }
                    };
                    if let Some(prev) = mined {
                        shared.broadcast_all(ServerMessage::BlockUpdate {
                            x,
                            y,
                            block: crate::block::AIR,
                        });
                        spawn_drop(&shared, x, y, prev);
                    }
                }
                ClientMessage::PlaceBlock { x, y, slot } => {
                    // A block may only be placed into an empty cell that is
                    // orthogonally adjacent to an existing block, so players build
                    // off the world rather than dropping blocks into open air.
                    let supported = {
                        let mut world = shared.world.lock();
                        world.get(x, y) == crate::block::AIR
                            && [(1, 0), (-1, 0), (0, 1), (0, -1)]
                                .iter()
                                .any(|(dx, dy)| world.get(x + dx, y + dy) != crate::block::AIR)
                    };
                    if !supported {
                        // Reject: resync the cell's true contents and the inventory
                        // so the client's optimistic placement is undone.
                        let actual = shared.world.lock().get(x, y);
                        shared.send_to(
                            id,
                            ServerMessage::BlockUpdate {
                                x,
                                y,
                                block: actual,
                            },
                        );
                        shared.send_inventory(id);
                        continue;
                    }
                    // Read the block to place from the player's own slot, so they
                    // can only place what they actually hold.
                    match shared.take_from_slot(id, slot as usize) {
                        Some(block) => {
                            let changed = shared.world.lock().set(x, y, block);
                            if changed {
                                shared.broadcast_all(ServerMessage::BlockUpdate { x, y, block });
                            } else {
                                // Cell was occupied: refund the spent block.
                                shared.add_item(id, block);
                            }
                            shared.send_inventory(id);
                        }
                        None => {
                            // Empty slot: correct the client's optimistic guess by
                            // resending the cell's true contents and inventory.
                            let actual = shared.world.lock().get(x, y);
                            shared.send_to(
                                id,
                                ServerMessage::BlockUpdate {
                                    x,
                                    y,
                                    block: actual,
                                },
                            );
                            shared.send_inventory(id);
                        }
                    }
                }
                ClientMessage::MoveItem { from, to } => {
                    shared.move_item(id, from as usize, to as usize);
                    shared.send_inventory(id);
                }
                ClientMessage::PlayerMove { x, y } => {
                    if let Some(e) = shared.entities.lock().get_mut(id) {
                        e.x = x;
                        e.y = y;
                    }
                    shared.broadcast_except(
                        id,
                        ServerMessage::EntityMoved {
                            id,
                            x,
                            y,
                            vx: 0.0,
                            vy: 0.0,
                        },
                    );
                }
                ClientMessage::Attack { target } => {
                    // Validate reach and, if good, compute the knockback shoving
                    // the target away from the attacker.
                    let knockback = {
                        let entities = shared.entities.lock();
                        match (entities.get(id), entities.get(target)) {
                            (Some(a), Some(b)) => {
                                let (aw, ah) = a.size();
                                let (bw, bh) = b.size();
                                if aabb_gap(a.x, a.y, aw, ah, b.x, b.y, bw, bh)
                                    <= PLAYER_ATTACK_REACH
                                {
                                    let dir = if b.x + bw * 0.5 >= a.x + aw * 0.5 {
                                        1.0
                                    } else {
                                        -1.0
                                    };
                                    Some((dir * KNOCKBACK_X, -KNOCKBACK_Y))
                                } else {
                                    None
                                }
                            }
                            _ => None,
                        }
                    };
                    if let Some(kb) = knockback {
                        let (msgs, respawn) = {
                            let mut entities = shared.entities.lock();
                            apply_damage(
                                &mut entities,
                                target,
                                PLAYER_ATTACK_DAMAGE,
                                kb,
                                shared.spawn,
                            )
                        };
                        for m in msgs {
                            shared.broadcast_all(m);
                        }
                        if let Some((rid, rx, ry)) = respawn {
                            shared.send_to(rid, ServerMessage::Respawn { x: rx, y: ry });
                        }
                    }
                }
                ClientMessage::FallDamage { amount } => {
                    if amount > 0 {
                        let (msgs, respawn) = {
                            let mut entities = shared.entities.lock();
                            apply_damage(&mut entities, id, amount, (0.0, 0.0), shared.spawn)
                        };
                        for m in msgs {
                            shared.broadcast_all(m);
                        }
                        if let Some((rid, rx, ry)) = respawn {
                            shared.send_to(rid, ServerMessage::Respawn { x: rx, y: ry });
                        }
                    }
                }
            }
        }
    }
    .await;

    // Cleanup. Preserve the player's state so they resume where they left off.
    shared.clients.lock().remove(&id);
    let removed = shared.entities.lock().remove(id);
    let inventory = shared.inventories.lock().remove(&id).unwrap_or_default();
    if let Some(Entity {
        kind: EntityKind::Player { name },
        x,
        y,
        health,
        ..
    }) = removed
        && !name.is_empty()
    {
        shared.saved_players.lock().insert(
            name.clone(),
            SavedPlayer {
                name,
                x,
                y,
                health,
                inventory,
            },
        );
    }
    shared.broadcast_all(ServerMessage::EntityDespawn { id });
    writer.abort();
    log::info!("player {id} disconnected");
    read_result
}

/// Default bind address for a publicly-hosted server.
pub fn host_bind(port: u16) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))
}

/// Loopback bind with an ephemeral port, for the embedded singleplayer server.
pub fn local_bind() -> SocketAddr {
    SocketAddr::from((Ipv4Addr::LOCALHOST, 0))
}
