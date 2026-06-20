//! Authoritative game server over QUIC (quinn).
//!
//! The server generates a fresh self-signed certificate on every launch. Its
//! fingerprint is surfaced to the client so the TOFU flow (see
//! [`crate::client::net`]) can pin it. For singleplayer the client embeds a
//! server in-process and auto-trusts that fingerprint.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use parking_lot::Mutex;
use quinn::Endpoint;
use tokio::sync::mpsc;

use crate::block::BlockRegistry;
use crate::entity::{Entities, Entity, EntityId, EntityKind, SLIME_SIZE};
use crate::net::{fingerprint, read_msg, write_msg};
use crate::protocol::{ALPN, BlockId, ClientMessage, ServerMessage};
use crate::world::{CHUNK_AREA, CHUNK_SIZE, TILE_SIZE, World};
use crate::worldgen::{WorldGen, spawn_point};

/// How often the server simulates non-player entities, in seconds.
const TICK_DT: f32 = 0.05;
/// Horizontal wander speed of a slime, in pixels/second.
const SLIME_SPEED: f32 = 30.0;
/// How many slimes to spawn near the world origin at startup.
const SLIME_COUNT: i32 = 4;
/// Downward acceleration applied to simulated entities, in pixels/second².
const GRAVITY: f32 = 1400.0;
/// Terminal fall speed for simulated entities, in pixels/second.
const MAX_FALL: f32 = 900.0;
/// Collision skin: keeps an entity's trailing edge from snapping into the next
/// cell when it is flush against a block boundary.
const EPS: f32 = 0.01;

/// Handle to a server running on its own thread + tokio runtime.
pub struct RunningServer {
    pub addr: SocketAddr,
    pub fingerprint: [u8; 32],
    // Keeping the endpoint alive keeps the server listening; dropping it (when
    // this handle is dropped) closes the server.
    _endpoint: Endpoint,
}

/// One connected client, as seen by the server. The client's player avatar
/// lives in [`Shared::entities`] under the same id; this just holds the channel
/// used to push messages to it.
struct ClientHandle {
    tx: mpsc::UnboundedSender<ServerMessage>,
}

/// Server world: stored chunks, the generator that fills them on demand, and a
/// block registry for solidity queries during entity collision.
struct ServerWorld {
    world: World,
    generator: WorldGen,
    registry: BlockRegistry,
}

impl ServerWorld {
    fn ensure(&mut self, cx: i32, cy: i32) {
        if !self.world.has_chunk((cx, cy)) {
            let chunk = self.generator.generate_chunk(cx, cy);
            self.world.insert_chunk((cx, cy), chunk);
        }
    }

    fn chunk_blocks(&mut self, cx: i32, cy: i32) -> Vec<BlockId> {
        self.ensure(cx, cy);
        self.world
            .get_chunk((cx, cy))
            .map(|c| c.blocks.to_vec())
            .unwrap_or_else(|| vec![0; CHUNK_AREA])
    }

    fn set(&mut self, x: i32, y: i32, b: BlockId) -> bool {
        let (cx, cy) = (x.div_euclid(CHUNK_SIZE), y.div_euclid(CHUNK_SIZE));
        self.ensure(cx, cy);
        self.world.set_block(x, y, b)
    }

    /// Surface (grass) row for a world column, used to place ground-walking
    /// entities when they first spawn.
    fn surface(&self, world_x: i32) -> i32 {
        self.generator.surface_height(world_x)
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
}

impl Shared {
    fn alloc_id(&self) -> EntityId {
        self.next_id.fetch_add(1, Ordering::SeqCst)
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
}

/// Start a server on a background thread. `bind` of port 0 picks an ephemeral
/// port (used for the embedded singleplayer server). Returns once the endpoint
/// is listening, with its actual address and certificate fingerprint.
pub fn start_server(bind: SocketAddr, seed: i32) -> Result<RunningServer> {
    let (ready_tx, ready_rx) =
        std::sync::mpsc::channel::<Result<(SocketAddr, [u8; 32], Endpoint)>>();

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
                let shared = match setup(bind, seed, &ready_tx).await {
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
        Ok((addr, fp, endpoint)) => Ok(RunningServer {
            addr,
            fingerprint: fp,
            _endpoint: endpoint,
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
    ready_tx: &std::sync::mpsc::Sender<Result<(SocketAddr, [u8; 32], Endpoint)>>,
) -> Option<AcceptCtx> {
    match build_endpoint(bind, seed) {
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
            let _ = ready_tx.send(Ok((addr, fp, endpoint.clone())));
            Some(AcceptCtx { endpoint, shared })
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            None
        }
    }
}

fn build_endpoint(bind: SocketAddr, seed: i32) -> Result<(Endpoint, [u8; 32], Arc<Shared>)> {
    // Self-signed certificate for "localhost".
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .context("generating self-signed certificate")?;
    let cert_der = cert.cert.der().clone();
    let fp = fingerprint(cert_der.as_ref());
    let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());

    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der.into())
        .context("building rustls server config")?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let qsc = quinn::crypto::rustls::QuicServerConfig::try_from(crypto)
        .context("building QUIC server config")?;
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(qsc));

    let endpoint = Endpoint::server(server_config, bind).context("binding server endpoint")?;

    let generator = WorldGen::new(seed);
    let spawn = spawn_point(&generator, 0);
    let shared = Arc::new(Shared {
        world: Mutex::new(ServerWorld {
            world: World::new(),
            generator,
            registry: BlockRegistry::new(),
        }),
        clients: Mutex::new(HashMap::new()),
        entities: Mutex::new(Entities::new()),
        next_id: AtomicU32::new(1),
        spawn,
    });

    spawn_slimes(&shared);

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

/// Populate the world with a handful of wandering slimes near the origin.
fn spawn_slimes(shared: &Shared) {
    let (_, h) = SLIME_SIZE;
    let world = shared.world.lock();
    let mut entities = shared.entities.lock();
    for i in 0..SLIME_COUNT {
        let cell_x = (i - SLIME_COUNT / 2) * 6;
        let surface = world.surface(cell_x);
        let id = shared.alloc_id();
        let x = cell_x as f32 * TILE_SIZE;
        let y = surface as f32 * TILE_SIZE - h;
        entities.insert(Entity::new(id, EntityKind::Slime, x, y));
    }
}

/// Periodically simulates non-player entities and broadcasts their motion.
async fn entity_tick_loop(shared: Arc<Shared>) {
    let mut interval = tokio::time::interval(Duration::from_secs_f32(TICK_DT));
    loop {
        interval.tick().await;
        for msg in step_entities(&shared) {
            shared.broadcast_all(msg);
        }
    }
}

/// Advance every server-simulated entity by one tick and return the movement
/// updates to broadcast. Players are skipped — they are authoritative on their
/// owning client.
///
/// Locks `world` then `entities` for the whole step, matching the order used by
/// [`spawn_slimes`] so the two can never deadlock.
fn step_entities(shared: &Shared) -> Vec<ServerMessage> {
    let mut world = shared.world.lock();
    let mut entities = shared.entities.lock();

    let ids: Vec<EntityId> = entities
        .values()
        .filter(|e| !e.kind.is_player())
        .map(|e| e.id)
        .collect();
    if ids.is_empty() {
        return Vec::new();
    }

    let mut updates = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // Keep walking in the current heading; spawn (vx == 0) starts rightward.
        let mut vx = if e.vx < 0.0 {
            -SLIME_SPEED
        } else {
            SLIME_SPEED
        };
        let mut vy = (e.vy + GRAVITY * TICK_DT).min(MAX_FALL);

        // Horizontal first, then vertical — each axis resolved independently.
        let (x, hit_wall) = move_x(&mut world, e.x, e.y, w, h, vx * TICK_DT);
        let (y, on_ground) = move_y(&mut world, x, e.y, w, h, vy * TICK_DT);
        if on_ground {
            vy = 0.0;
        }
        // Turn around at walls, and at ledges so slimes patrol rather than
        // walk off into the infinite distance.
        if hit_wall || (on_ground && at_ledge(&mut world, x, y, w, h, vx)) {
            vx = -vx;
        }

        e.x = x;
        e.y = y;
        e.vx = vx;
        e.vy = vy;
        updates.push(ServerMessage::EntityMoved { id, x, y, vx, vy });
    }
    updates
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

/// Whether a grounded entity heading in direction `vx` is at a ledge — i.e.
/// there is no solid ground under the cell just ahead of its leading foot.
fn at_ledge(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, vx: f32) -> bool {
    let ahead = if vx > 0.0 { x + w + EPS } else { x - EPS };
    let tx = (ahead / TILE_SIZE).floor() as i32;
    let ty = ((y + h + EPS) / TILE_SIZE).floor() as i32;
    !world.solid(tx, ty)
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
    log::info!("player {id} connected");

    // Reader loop.
    let read_result: Result<()> = async {
        loop {
            let msg: ClientMessage = read_msg(&mut recv).await?;
            match msg {
                ClientMessage::Hello { name } => {
                    log::info!("player {id} is '{name}'");
                    if let Some(e) = shared.entities.lock().get_mut(id) {
                        e.kind = EntityKind::Player { name };
                    }
                }
                ClientMessage::RequestChunk { cx, cy } => {
                    let blocks = shared.world.lock().chunk_blocks(cx, cy);
                    let _ = tx.send(ServerMessage::Chunk { cx, cy, blocks });
                }
                ClientMessage::SetBlock { x, y, block } => {
                    let changed = shared.world.lock().set(x, y, block);
                    if changed {
                        shared.broadcast_all(ServerMessage::BlockUpdate { x, y, block });
                    }
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
            }
        }
    }
    .await;

    // Cleanup.
    shared.clients.lock().remove(&id);
    shared.entities.lock().remove(id);
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
