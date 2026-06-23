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
use crate::entity::{BONE_SIZE, Entities, Entity, EntityId, EntityKind, ITEM_SIZE, PLAYER_SIZE};
use crate::inventory::Inventory;
use crate::net::{VERSION_MISMATCH_CLOSE, fingerprint, read_msg, read_version, write_msg};
use crate::protocol::{ALPN, BlockId, ClientMessage, PROTOCOL_VERSION, ServerMessage, Waypoint};
use crate::save::{SavedPlayer, WorldMeta, WorldStore};
use crate::world::{CHUNK_AREA, CHUNK_SIZE, ChunkCoord, TILE_SIZE, WORLD_HEIGHT, World};
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
/// Quick scuttling speed of a spider, in pixels/second — faster than anything
/// else that walks, so it closes distance and a player can't simply outrun it.
const SPIDER_SPEED: f32 = 46.0;
/// Speed (px/s) at which a chasing spider scales a wall it has run into. Matched
/// to its ground speed so climbing never stalls the pursuit.
const SPIDER_CLIMB_SPEED: f32 = 46.0;
/// How far (px) a spider notices a player and gives chase.
const SPIDER_AGGRO: f32 = 180.0;
/// Maximum gap (px between AABBs) at which a spider can land a bite.
const SPIDER_ATTACK_RANGE: f32 = 4.0;
/// Damage a spider deals per bite.
const SPIDER_DAMAGE: i32 = 4;
/// Seconds a spider waits between bites.
const SPIDER_ATTACK_INTERVAL: f32 = 0.9;
/// Percent chance that a fresh, eligible chunk (forest surface, or anywhere deep
/// underground) seeds spiders.
const SPIDER_CHUNK_CHANCE: u32 = 35;
/// Most spiders a single eligible chunk seeds at once.
const SPIDER_CHUNK_MAX: u32 = 2;
/// World row at or below which a chunk counts as the deep dark spiders haunt, so
/// they only nest in the underground caverns and never in surface tunnels that
/// open to daylight. Sits well below the surface baseline (`WORLD_HEIGHT / 2`).
const SPIDER_CAVERN_MIN_Y: i32 = WORLD_HEIGHT * 11 / 16;
/// Stalking speed of a skeleton, in pixels/second — a touch quicker than a
/// zombie, since it wants to reposition for a clean shot rather than just maul.
const SKELETON_SPEED: f32 = 22.0;
/// How far (px) a skeleton notices a player and begins stalking/firing.
const SKELETON_AGGRO: f32 = 240.0;
/// Maximum gap (px between AABBs) at which a skeleton will loose a bone — it
/// stops advancing and throws once a player is this close.
const SKELETON_THROW_RANGE: f32 = 190.0;
/// Standoff gap (px between AABBs) a skeleton tries to keep: it backs away from a
/// player closer than this so it can keep peppering them from range.
const SKELETON_KEEP_DIST: f32 = 90.0;
/// Seconds a skeleton waits between throws.
const SKELETON_THROW_INTERVAL: f32 = 1.6;
/// Of the night undead the server spawns near players, the percent that arrive as
/// skeletons rather than zombies.
const SKELETON_SPAWN_PERCENT: u32 = 30;
/// Flight speed of a thrown bone, in pixels/second.
const BONE_SPEED: f32 = 170.0;
/// Damage a bone deals on striking a player.
const BONE_DAMAGE: i32 = 5;
/// Seconds a thrown bone stays airborne before it gives out and despawns, in case
/// it never hits anything.
const BONE_LIFETIME: f32 = 3.0;
/// Maximum gap (px between AABBs) at which an in-flight bone counts as striking a
/// player.
const BONE_HIT_RANGE: f32 = 2.0;
/// Distance (px) from the nearest player beyond which any non-player entity is
/// culled, so creatures and dropped items don't pile up in terrain no one is
/// near. Set comfortably past the screen edge and every spawn distance, so an
/// entity never vanishes in view or pops away the instant it spawns.
const DESPAWN_DIST: f32 = 1200.0;
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
/// Player melee reach (max gap, px, between attacker and target AABBs).
const PLAYER_ATTACK_REACH: f32 = 80.0;
/// Horizontal knockback speed (px/s) shoved onto whatever a hit lands on, away
/// from the attacker.
const KNOCKBACK_X: f32 = 180.0;
/// Upward knockback speed (px/s) — a small pop so a hit lifts the target a touch.
const KNOCKBACK_Y: f32 = 240.0;
/// How often the server broadcasts the current time of day, in seconds.
const TIME_BROADCAST_SECS: f32 = 2.0;
/// How often spreading water flows one cell outward, in seconds. Slower than the
/// entity tick so a poured bucket creeps out at a watery pace rather than
/// snapping across the map in a single frame.
const WATER_FLOW_SECS: f32 = 0.3;
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
/// Horizontal speed (px/s) given to an item a player deliberately drops, tossed
/// in their facing direction so it lands a little away from them (and so a
/// discarded item doesn't immediately slide back underfoot).
const ITEM_DROP_VELOCITY_X: f32 = 90.0;
/// Seconds a player-dropped item must lie around before anyone (including the
/// dropper) can collect it. Longer than [`ITEM_PICKUP_DELAY`] so the dropper has
/// a moment to step away when discarding or gifting.
const ITEM_DROP_PICKUP_DELAY: f32 = 1.0;
/// How often the world is flushed to disk while running, in seconds.
const AUTOSAVE_SECS: f32 = 30.0;
/// Chebyshev distance (in cells) within which a leaf must find a log to survive.
/// When the last log in range is removed, the leaf decays (breaking, but still
/// dropping its items). Comfortably covers a generated tree's canopy.
const LEAF_SUPPORT_RANGE: i32 = 3;
/// Greatest length (in cells) one placed rope ladder unrolls before its rope
/// runs out. A deeper shaft needs a second rope ladder dropped onto the bottom
/// of the first to carry on down.
const ROPE_LADDER_MAX_DROP: i32 = 8;
/// Longest chat line the server will relay, in characters; longer lines are
/// truncated so a peer can't flood others with a huge message.
const MAX_CHAT_LEN: usize = 256;

/// Handle to a server running on its own thread + tokio runtime.
pub struct RunningServer {
    pub addr: SocketAddr,
    pub fingerprint: [u8; 32],
    /// The dev secret this server will accept in `Hello` to authorize dev mode.
    /// Passed to the creator's own client so only it can use dev tools.
    pub dev_token: u64,
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

    /// Pour a water *source* at `(x, y)` (flow distance 0, so it spreads — see
    /// [`World::place_water_source`]), generating the chunk if needed and marking
    /// it dirty. Returns whether it was placed.
    fn place_water(&mut self, x: i32, y: i32) -> bool {
        let (cx, cy) = (x.div_euclid(CHUNK_SIZE), y.div_euclid(CHUNK_SIZE));
        self.ensure(cx, cy);
        let placed = self.world.place_water_source(x, y);
        if placed {
            self.dirty.insert((cx, cy));
        }
        placed
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

    /// Flow resident water one cell outward (see [`World::spread_water_once`]),
    /// marking every newly filled cell's chunk dirty so the flood is saved, and
    /// returning those cells for broadcast.
    fn spread_water(&mut self) -> Vec<(i32, i32)> {
        let filled = self.world.spread_water_once();
        for &(x, y) in &filled {
            self.dirty
                .insert((x.div_euclid(CHUNK_SIZE), y.div_euclid(CHUNK_SIZE)));
        }
        filled
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
    /// load so a resumed world keeps the time of day it was saved at, and movable
    /// at runtime by dev mode's `SetTime` (hence the `Mutex`).
    start: Mutex<Instant>,
    /// Per-server dev secret. Handed only to the creator's in-process client (via
    /// [`RunningServer::dev_token`]); a connection that presents it in `Hello` is
    /// authorized for dev-mode commands. Never sent to other clients, so a remote
    /// joiner cannot guess it and grant itself dev powers.
    dev_token: u64,
    /// Saved state of every player who has joined, keyed by name. A player is
    /// moved out of here (into a live entity) while connected and folded back in
    /// on disconnect, so it survives both reconnects and restarts.
    saved_players: Mutex<HashMap<String, SavedPlayer>>,
    /// Slot inventory of every currently-connected player, keyed by entity id.
    /// Authoritative: placements consume from it and pickups add to it. Folded
    /// into [`SavedPlayer`] on disconnect so it persists.
    inventories: Mutex<HashMap<EntityId, Inventory>>,
    /// Campfire cell each connected player last interacted with, keyed by entity
    /// id. Death returns the player to this campfire (instead of world
    /// [`spawn`](Self::spawn)) — but only if the campfire is still there; a broken
    /// one falls back to world spawn. Folded into [`SavedPlayer`] on disconnect so
    /// it survives reconnects.
    respawn_points: Mutex<HashMap<EntityId, (i32, i32)>>,
    /// Personal map waypoints of every currently-connected player, keyed by
    /// entity id. Folded into [`SavedPlayer`] on disconnect so they persist.
    waypoints: Mutex<HashMap<EntityId, Vec<Waypoint>>>,
    /// Lit campfires, keyed by world cell, holding each one's remaining burn time
    /// in seconds. A cell is present only while its campfire is lit; the tick loop
    /// counts each down and extinguishes it (reverting the block) at zero.
    /// Persisted in [`WorldMeta`] so fires survive a save/reload.
    campfires: Mutex<HashMap<(i32, i32), f32>>,
    /// Cells holding a log the player placed (rather than naturally grown).
    /// Tracked so an axe's tree-felling leaves player-built log structures
    /// standing — a placed log is neither chopped nor traversed by the fell.
    /// Persisted in [`WorldMeta`] so the distinction survives a save/reload.
    placed_logs: Mutex<HashSet<(i32, i32)>>,
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
        daylight::time_of_day(self.start.lock().elapsed().as_secs_f32())
    }

    /// A pseudo-random value in `[0, 1)`, mixed from the entity counter and the
    /// sub-second clock so successive rolls (e.g. for randomized leaf drops) vary.
    /// Not for anything that needs to be reproducible.
    fn rand_unit(&self) -> f32 {
        let a = self.next_id.load(Ordering::Relaxed);
        let b = self.start.lock().elapsed().subsec_nanos();
        let mut h = a
            .wrapping_mul(2_654_435_761)
            .wrapping_add(b.wrapping_mul(40_503));
        h ^= h >> 15;
        h = h.wrapping_mul(0x2c1b_3c6d);
        h ^= h >> 13;
        (h & 0x00FF_FFFF) as f32 / 16_777_216.0
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
                            respawn: self.respawn_points.lock().get(&e.id).copied(),
                            waypoints: self
                                .waypoints
                                .lock()
                                .get(&e.id)
                                .cloned()
                                .unwrap_or_default(),
                        },
                    );
                }
                EntityKind::Player { .. } => {} // unnamed: not yet identified
                _ => creatures.push(e.clone()),
            }
        }

        let campfires = self
            .campfires
            .lock()
            .iter()
            .map(|(&(x, y), &secs)| (x, y, secs))
            .collect();
        let placed_logs = self.placed_logs.lock().iter().copied().collect();

        let meta = WorldMeta {
            seed,
            elapsed_secs: self.start.lock().elapsed().as_secs_f32(),
            next_id: self.next_id.load(Ordering::SeqCst),
            spawn: self.spawn,
            entities: creatures,
            players: players.into_values().collect(),
            campfires,
            placed_logs,
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

    /// Add a dropped stack (`count` of `block` carrying `durability`) to player
    /// `id`'s inventory, preserving a tool's wear. Returns the amount that did
    /// not fit (0 when all of it was stored).
    fn add_stack(&self, id: EntityId, block: BlockId, count: u32, durability: u16) -> u32 {
        self.inventories
            .lock()
            .entry(id)
            .or_default()
            .add_stack(block, count, durability)
    }

    /// Drop the contents of player `id`'s inventory `slot` onto the ground at
    /// their feet so it can be discarded or gifted. `all` drops the whole stack;
    /// otherwise a single item is dropped. `dir` is the player's facing
    /// (`-1.0` left, `+1.0` right) so the drop is tossed clear of them. The drop
    /// keeps a tool's durability. No-op if the slot is empty or the player has no
    /// position yet.
    fn drop_item(&self, id: EntityId, slot: usize, all: bool, dir: f32) {
        let taken = {
            let mut invs = self.inventories.lock();
            let Some(inv) = invs.get_mut(&id) else {
                return;
            };
            if all {
                inv.take_slot(slot)
            } else {
                inv.take_one_full(slot)
            }
        };
        let Some((block, count, durability)) = taken else {
            return;
        };
        // Spawn at the player's center so the toss reads as coming from them.
        let origin = self.entities.lock().get(id).map(|e| {
            let (pw, ph) = e.size();
            let (iw, ih) = ITEM_SIZE;
            (e.x + (pw - iw) * 0.5, e.y + (ph - ih) * 0.5)
        });
        let Some((x, y)) = origin else {
            return;
        };
        let vx = if dir < 0.0 {
            -ITEM_DROP_VELOCITY_X
        } else {
            ITEM_DROP_VELOCITY_X
        };
        spawn_item(
            self,
            block,
            count,
            durability,
            x,
            y,
            vx,
            ITEM_POP_VELOCITY,
            ITEM_DROP_PICKUP_DELAY,
        );
    }

    /// Remove one item from hotbar/inventory `slot` of player `id`, returning the
    /// block taken (or `None` if the slot was empty). Used to pay for placement.
    fn take_from_slot(&self, id: EntityId, slot: usize) -> Option<BlockId> {
        self.inventories.lock().get_mut(&id)?.take_one(slot)
    }

    /// Read (without removing) the block in hotbar/inventory `slot` of player
    /// `id`. Used to validate placement before committing to spend the item.
    fn peek_slot(&self, id: EntityId, slot: usize) -> Option<BlockId> {
        self.inventories.lock().get(&id)?.get(slot).map(|(b, ..)| b)
    }

    /// Rearrange player `id`'s inventory by moving slot `from` onto slot `to`.
    fn move_item(&self, id: EntityId, from: usize, to: usize) {
        if let Some(inv) = self.inventories.lock().get_mut(&id) {
            inv.move_stack(from, to);
        }
    }

    /// Apply one execution of `recipe` to player `id`: if they hold every input,
    /// consume the inputs and grant the outputs, spilling any overflow at their
    /// feet. Returns whether the recipe was applied (false if materials were
    /// insufficient).
    fn apply_recipe(&self, id: EntityId, recipe: &crate::recipe::Recipe) -> bool {
        self.apply_recipe_with(id, recipe, &[])
    }

    /// As [`apply_recipe`](Self::apply_recipe), but requires and consumes `extra`
    /// inputs (as `(item, count)` pairs) on top of the recipe's own — used by the
    /// forge to burn a separately-chosen fuel alongside the smelt. The recipe is
    /// applied only if the player holds both the recipe inputs and every `extra`.
    fn apply_recipe_with(
        &self,
        id: EntityId,
        recipe: &crate::recipe::Recipe,
        extra: &[(BlockId, u32)],
    ) -> bool {
        let overflow = {
            let mut invs = self.inventories.lock();
            let inv = invs.entry(id).or_default();
            if !recipe.craftable(inv) || extra.iter().any(|(item, n)| inv.count(*item) < *n) {
                return false;
            }
            for (item, n) in recipe.inputs {
                inv.remove(*item, *n);
            }
            for (item, n) in extra {
                inv.remove(*item, *n);
            }
            let mut overflow = Vec::new();
            for (item, n) in recipe.outputs {
                let left = inv.add(*item, *n);
                if left > 0 {
                    overflow.push((*item, left));
                }
            }
            overflow
        };
        // Spill anything that didn't fit at the crafter's location.
        if !overflow.is_empty() {
            let cell = self
                .entities
                .lock()
                .get(id)
                .map(|e| ((e.x / TILE_SIZE) as i32, (e.y / TILE_SIZE) as i32));
            if let Some((cx, cy)) = cell {
                for (item, n) in overflow {
                    for _ in 0..n {
                        spawn_drop(self, cx, cy, item);
                    }
                }
            }
        }
        true
    }

    /// Craft `RECIPES[recipe_idx]` once for player `id`. No-op for an unknown
    /// recipe or insufficient materials.
    fn craft(&self, id: EntityId, recipe_idx: usize) {
        if let Some(recipe) = crate::recipe::RECIPES.get(recipe_idx) {
            self.apply_recipe(id, recipe);
        }
    }

    /// Smelt `SMELT_RECIPES[recipe_idx]` up to `count` times for player `id`,
    /// burning `fuel` (a [`forge_fuel_units`](crate::block::forge_fuel_units) charge
    /// per smelt) and stopping as soon as the raw material or fuel runs out. No-op
    /// for an unknown recipe or an item that can't fuel a forge.
    fn smelt(&self, id: EntityId, recipe_idx: usize, count: u32, fuel: BlockId) {
        let Some(recipe) = crate::recipe::SMELT_RECIPES.get(recipe_idx) else {
            return;
        };
        let Some(units) = crate::block::forge_fuel_units(fuel) else {
            return;
        };
        for _ in 0..count {
            if !self.apply_recipe_with(id, recipe, &[(fuel, units)]) {
                break;
            }
        }
    }

    /// Repair one worn `item` tool for player `id`, restoring
    /// [`repair_step`](crate::block::repair_step) durability in exchange for one
    /// unit of its [`repair_material`](crate::block::repair_material). No-op if
    /// the item isn't repairable, the player holds no such damaged tool, or they
    /// lack the material.
    fn repair(&self, id: EntityId, item: BlockId) {
        let Some(material) = crate::block::repair_material(item) else {
            return;
        };
        let mut invs = self.inventories.lock();
        let inv = invs.entry(id).or_default();
        if inv.count(material) == 0 {
            return;
        }
        // Only spend material if a damaged tool was actually mended.
        if inv.repair_tool(item, crate::block::repair_step(item)) {
            inv.remove(material, 1);
        }
    }

    /// Spend `wear` durability on player `id`'s held `tool`, broadcasting nothing
    /// but pushing the owner a fresh inventory snapshot so the durability bar (and
    /// a now-broken tool's empty slot) updates. No-op when `wear` is zero.
    fn wear_tool(&self, id: EntityId, tool: BlockId, wear: u16) {
        if wear == 0 {
            return;
        }
        {
            let mut invs = self.inventories.lock();
            invs.entry(id).or_default().damage_tool(tool, wear);
        }
        self.send_inventory(id);
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

    /// Eat the food in player `id`'s inventory `slot`: consume one and adjust the
    /// player's health by its [`food_heal`](crate::block::food_heal). A positive
    /// amount heals (capped at the player's maximum); a negative one (raw meat)
    /// damages and can even kill. Returns the health/hit messages to broadcast and
    /// a respawn target if the bite proved fatal. No-op (empty) if the slot holds
    /// no food.
    fn eat(&self, id: EntityId, slot: usize) -> (Vec<ServerMessage>, Option<(EntityId, f32, f32)>) {
        let Some(item) = self.peek_slot(id, slot) else {
            return (Vec::new(), None);
        };
        let Some(delta) = crate::block::food_heal(item) else {
            return (Vec::new(), None);
        };
        // Spend one serving before applying its effect.
        {
            let mut invs = self.inventories.lock();
            let Some(inv) = invs.get_mut(&id) else {
                return (Vec::new(), None);
            };
            if inv.take_one(slot).is_none() {
                return (Vec::new(), None);
            }
        }
        let mut entities = self.entities.lock();
        if delta < 0 {
            // Raw meat: route through the damage path so a fatal bite respawns.
            apply_damage(
                &mut entities,
                id,
                -delta,
                (0.0, 0.0),
                self.respawn_target(id),
            )
        } else {
            let Some(e) = entities.get_mut(id) else {
                return (Vec::new(), None);
            };
            e.health = (e.health + delta).min(e.max_health);
            (
                vec![ServerMessage::EntityHealth {
                    id,
                    health: e.health,
                    max_health: e.max_health,
                }],
                None,
            )
        }
    }

    /// Feed one unit of `fuel` to the campfire at cell `(x, y)` for player `id`:
    /// light it (if unlit) and extend its burn time. No-op unless the cell is a
    /// campfire, `fuel` is valid fuel, and the player holds some.
    fn fuel_campfire(&self, id: EntityId, x: i32, y: i32, fuel: BlockId) {
        let Some(secs) = crate::block::fuel_seconds(fuel) else {
            return;
        };
        if !crate::block::is_campfire(self.world.lock().get(x, y)) {
            return;
        }
        // Spend one unit of fuel.
        {
            let mut invs = self.inventories.lock();
            let inv = invs.entry(id).or_default();
            if inv.count(fuel) == 0 {
                return;
            }
            inv.remove(fuel, 1);
        }
        // Stoke the fire (extending an already-burning one).
        *self.campfires.lock().entry((x, y)).or_insert(0.0) += secs;
        // Light it if it wasn't already, telling everyone its new lit look.
        let lit = {
            let mut world = self.world.lock();
            world.get(x, y) == crate::block::CAMPFIRE && world.set(x, y, crate::block::CAMPFIRE_LIT)
        };
        if lit {
            self.broadcast_all(ServerMessage::BlockUpdate {
                x,
                y,
                block: crate::block::CAMPFIRE_LIT,
            });
        }
        self.send_inventory(id);
    }

    /// Use the bucket in player `id`'s inventory `slot` on world cell `(x, y)`: an
    /// empty [`BUCKET`](crate::block::BUCKET) scoops up a [`WATER`](crate::block::WATER)
    /// cell (becoming a [`WATER_BUCKET`](crate::block::WATER_BUCKET)), and a water
    /// bucket pours its load into an empty cell (becoming empty again). The held
    /// item, the target cell, and inventory room are all validated; on any mismatch
    /// the cell and inventory are resynced so a client's optimistic guess is undone.
    fn use_bucket(&self, id: EntityId, x: i32, y: i32, slot: usize) {
        let held = self.peek_slot(id, slot);
        // Read the target cell and whether it has an orthogonal neighbour to rest
        // against — the same support a normal block placement requires, so water
        // can't be poured into open midair.
        let (cell, supported) = {
            let mut world = self.world.lock();
            let cell = world.get(x, y);
            let supported = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                .iter()
                .any(|(dx, dy)| world.get(x + dx, y + dy) != crate::block::AIR);
            (cell, supported)
        };
        let changed = match held {
            // Scoop: an empty bucket fills from a water cell.
            Some(crate::block::BUCKET) if crate::block::is_water(cell) => {
                self.take_from_slot(id, slot);
                self.world.lock().set(x, y, crate::block::AIR);
                // Hand back a water bucket; if the inventory is somehow full, undo.
                if self.add_item(id, crate::block::WATER_BUCKET) {
                    Some(crate::block::AIR)
                } else {
                    self.world.lock().set(x, y, crate::block::WATER);
                    self.add_item(id, crate::block::BUCKET);
                    None
                }
            }
            // Pour: a water bucket empties into an open, supported cell (never
            // into midair, mirroring normal block placement).
            Some(crate::block::WATER_BUCKET) if cell == crate::block::AIR && supported => {
                self.take_from_slot(id, slot);
                if self.world.lock().place_water(x, y) {
                    // Return the now-empty bucket; if it can't fit, spill it.
                    if !self.add_item(id, crate::block::BUCKET) {
                        spawn_drop(self, x, y, crate::block::BUCKET);
                    }
                    Some(crate::block::WATER)
                } else {
                    self.add_item(id, crate::block::WATER_BUCKET); // refund
                    None
                }
            }
            _ => None,
        };
        match changed {
            Some(block) => self.broadcast_all(ServerMessage::BlockUpdate { x, y, block }),
            // Nothing happened: correct the client's optimistic cell guess.
            None => {
                let actual = self.world.lock().get(x, y);
                self.send_to(
                    id,
                    ServerMessage::BlockUpdate {
                        x,
                        y,
                        block: actual,
                    },
                );
            }
        }
        self.send_inventory(id);
    }

    /// Record the campfire at cell `(x, y)` as player `id`'s respawn point. No-op
    /// unless that cell really holds a campfire.
    fn set_respawn(&self, id: EntityId, x: i32, y: i32) {
        if !crate::block::is_campfire(self.world.lock().get(x, y)) {
            return;
        }
        self.respawn_points.lock().insert(id, (x, y));
    }

    /// Where player `id` should respawn: their last campfire if they've set one and
    /// it's still standing, otherwise world [`spawn`](Self::spawn) (so a broken
    /// campfire sends them back to spawn).
    fn respawn_target(&self, id: EntityId) -> (f32, f32) {
        let Some((cx, cy)) = self.respawn_points.lock().get(&id).copied() else {
            return self.spawn;
        };
        if !crate::block::is_campfire(self.world.lock().get(cx, cy)) {
            return self.spawn;
        }
        // Centre the player's 11px-wide body in the campfire's tile; its top sits
        // a tile up so its feet rest on the ground the campfire stands on.
        let px = cx as f32 * TILE_SIZE + (TILE_SIZE - crate::entity::PLAYER_SIZE.0) / 2.0;
        let py = cy as f32 * TILE_SIZE;
        (px, py)
    }

    /// Send player `id` the authoritative snapshot of their waypoints and current
    /// home (respawn) point, so the client can redraw its markers.
    fn send_waypoints(&self, id: EntityId) {
        let list = self.waypoints.lock().get(&id).cloned().unwrap_or_default();
        let home = self.respawn_target(id);
        self.send_to(id, ServerMessage::Waypoints { list, home });
    }

    /// Record a personal waypoint for player `id`, then resync their list.
    fn add_waypoint(&self, id: EntityId, wp: Waypoint) {
        self.waypoints.lock().entry(id).or_default().push(wp);
        self.send_waypoints(id);
    }

    /// Drop player `id`'s waypoint nearest to world pixel `(x, y)`, then resync.
    /// No-op if they have none.
    fn remove_waypoint(&self, id: EntityId, x: f32, y: f32) {
        {
            let mut all = self.waypoints.lock();
            let Some(list) = all.get_mut(&id) else {
                return;
            };
            let nearest = list
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    let da = (a.x - x).powi(2) + (a.y - y).powi(2);
                    let db = (b.x - x).powi(2) + (b.y - y).powi(2);
                    da.total_cmp(&db)
                })
                .map(|(i, _)| i);
            if let Some(i) = nearest {
                list.remove(i);
            }
        }
        self.send_waypoints(id);
    }

    /// Cook `COOK_RECIPES[recipe_idx]` up to `count` times for player `id` on the
    /// campfire at cell `(x, y)`, stopping when the inputs run out. No-op unless
    /// that campfire is lit.
    fn cook(&self, id: EntityId, x: i32, y: i32, recipe_idx: usize, count: u32) {
        if self.world.lock().get(x, y) != crate::block::CAMPFIRE_LIT {
            return;
        }
        if let Some(recipe) = crate::recipe::COOK_RECIPES.get(recipe_idx) {
            for _ in 0..count {
                if !self.apply_recipe(id, recipe) {
                    break;
                }
            }
        }
        self.send_inventory(id);
    }

    /// Flow spreading water one cell outward and return the resulting block
    /// updates to broadcast. Each freshly flooded cell becomes a [`WATER`] block.
    fn tick_water(&self) -> Vec<ServerMessage> {
        self.world
            .lock()
            .spread_water()
            .into_iter()
            .map(|(x, y)| ServerMessage::BlockUpdate {
                x,
                y,
                block: crate::block::WATER,
            })
            .collect()
    }

    /// Advance every lit campfire by `dt`, dropping any whose underlying block is
    /// gone and extinguishing any whose fuel has run out (reverting the cell to an
    /// unlit campfire). Returns the resulting block updates to broadcast.
    fn tick_campfires(&self, dt: f32) -> Vec<ServerMessage> {
        let mut expired = Vec::new();
        {
            let mut world = self.world.lock();
            let mut fires = self.campfires.lock();
            fires.retain(|&(x, y), secs| {
                if !crate::block::is_campfire(world.get(x, y)) {
                    return false; // the campfire was mined or overwritten
                }
                *secs -= dt;
                if *secs <= 0.0 {
                    expired.push((x, y));
                    false
                } else {
                    true
                }
            });
        }
        let mut msgs = Vec::new();
        if !expired.is_empty() {
            let mut world = self.world.lock();
            for (x, y) in expired {
                if world.get(x, y) == crate::block::CAMPFIRE_LIT
                    && world.set(x, y, crate::block::CAMPFIRE)
                {
                    msgs.push(ServerMessage::BlockUpdate {
                        x,
                        y,
                        block: crate::block::CAMPFIRE,
                    });
                }
            }
        }
        msgs
    }

    /// Fell the rest of a tree: flood-fill the run of logs 4-connected to the
    /// (already-cleared) cell `(x, y)`, clearing each and returning their cells so
    /// the caller can drop them. Naturally-grown logs only — a player-placed log
    /// (tracked in [`Shared::placed_logs`]) is left standing and also blocks the
    /// spread, so building with logs never collapses an adjacent tree and felling
    /// a tree never eats a player's log structure.
    fn chop_connected_logs(&self, x: i32, y: i32) -> Vec<(i32, i32)> {
        let mut world = self.world.lock();
        let placed = self.placed_logs.lock();
        let mut removed = Vec::new();
        let mut seen: HashSet<(i32, i32)> = HashSet::new();
        seen.insert((x, y)); // origin already cleared by the caller
        let mut stack: Vec<(i32, i32)> = [(1, 0), (-1, 0), (0, 1), (0, -1)]
            .iter()
            .map(|(dx, dy)| (x + dx, y + dy))
            .collect();
        while let Some((cx, cy)) = stack.pop() {
            if !seen.insert((cx, cy)) {
                continue;
            }
            if world.get(cx, cy) != crate::block::LOG || placed.contains(&(cx, cy)) {
                continue;
            }
            world.set(cx, cy, crate::block::AIR);
            removed.push((cx, cy));
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                stack.push((cx + dx, cy + dy));
            }
        }
        removed
    }

    /// Decay leaves left unsupported after the logs at `removed_logs` were
    /// cleared: any leaf within [`LEAF_SUPPORT_RANGE`] of a removed log that no
    /// longer has a log in range is broken. Returns each broken leaf's cell and
    /// its rolled drop (leaves still shed their items as they decay).
    fn decay_unsupported_leaves(
        &self,
        removed_logs: &[(i32, i32)],
    ) -> Vec<(i32, i32, Option<BlockId>)> {
        let r = LEAF_SUPPORT_RANGE;
        // Every leaf that *could* have lost its only support sits within range of
        // a removed log; gather that neighbourhood (deduped) as the candidates.
        let mut candidates: HashSet<(i32, i32)> = HashSet::new();
        for &(lx, ly) in removed_logs {
            for dy in -r..=r {
                for dx in -r..=r {
                    candidates.insert((lx + dx, ly + dy));
                }
            }
        }
        let mut world = self.world.lock();
        let mut decayed = Vec::new();
        for (cx, cy) in candidates {
            if world.get(cx, cy) != crate::block::LEAVES || leaf_supported(&mut world, cx, cy) {
                continue;
            }
            world.set(cx, cy, crate::block::AIR);
            let drop = crate::block::mined_drop_rolled(crate::block::LEAVES, self.rand_unit());
            decayed.push((cx, cy, drop));
        }
        decayed
    }

    /// Unroll a rope ladder downward from `(x, y)`: fill that cell and each open
    /// cell directly beneath it with rope ladder, stopping at the first
    /// obstruction (the cave floor), the world's bottom, or after
    /// [`ROPE_LADDER_MAX_DROP`] cells (the rope running out). Returns the filled
    /// cells, top-first, so the caller can broadcast them. `(x, y)` is assumed to
    /// already be air and validated for support.
    fn roll_rope_ladder(&self, x: i32, y: i32) -> Vec<(i32, i32)> {
        let mut world = self.world.lock();
        let mut filled = Vec::new();
        let mut ty = y;
        while (filled.len() as i32) < ROPE_LADDER_MAX_DROP {
            if !crate::world::in_bounds(x, ty) || world.get(x, ty) != crate::block::AIR {
                break;
            }
            if !world.set(x, ty, crate::block::ROPE_LADDER) {
                break;
            }
            filled.push((x, ty));
            ty += 1;
        }
        filled
    }

    /// Reel in a whole rope ladder: clear every rope ladder cell vertically
    /// connected to `(x, y)` (the run runs up and down from there), returning the
    /// cells cleared so the caller can broadcast them. `(x, y)` itself has already
    /// been broken by the caller; this gathers the rest of the dangling run, which
    /// collapses as a unit rather than leaving floating rungs (and so a multi-cell
    /// run yields a single dropped rope ladder, not one per cell).
    fn collapse_rope_ladder(&self, x: i32, y: i32) -> Vec<(i32, i32)> {
        let mut world = self.world.lock();
        let mut cleared = Vec::new();
        for dir in [-1, 1] {
            let mut ty = y + dir;
            while crate::world::in_bounds(x, ty)
                && world.get(x, ty) == crate::block::ROPE_LADDER
                && world.set(x, ty, crate::block::AIR)
            {
                cleared.push((x, ty));
                ty += dir;
            }
        }
        cleared
    }
}

/// Whether a log lies within [`LEAF_SUPPORT_RANGE`] (Chebyshev) of cell
/// `(x, y)` — i.e. whether a leaf there still has nearby wood to cling to.
fn leaf_supported(world: &mut ServerWorld, x: i32, y: i32) -> bool {
    let r = LEAF_SUPPORT_RANGE;
    for dy in -r..=r {
        for dx in -r..=r {
            if world.get(x + dx, y + dy) == crate::block::LOG {
                return true;
            }
        }
    }
    false
}

/// Items a slain creature of `kind` drops, as `(item, count)` pairs. Animals
/// (chickens, goats) drop raw meat; everything else drops nothing.
fn creature_loot(kind: &EntityKind) -> &'static [(BlockId, u32)] {
    match kind {
        EntityKind::Chicken => &[(crate::block::RAW_MEAT, 1)],
        EntityKind::Goat => &[(crate::block::RAW_MEAT, 2)],
        _ => &[],
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
            dev_token: shared.dev_token,
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

    // Mint a per-server dev secret from the wall clock (mixed with next_id so two
    // servers started in the same instant still differ). It is never broadcast,
    // so a remote client can't learn it; only the creator's in-process client is
    // handed it via RunningServer.
    let dev_token = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xD157_C0DE)
        ^ (next_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);

    // Restore saved creatures into the live world; players go to the saved set.
    let mut entities = Entities::new();
    let mut saved_players = HashMap::new();
    let mut campfires = HashMap::new();
    let mut placed_logs = HashSet::new();
    if let Some(m) = &saved {
        for e in &m.entities {
            entities.insert(e.clone());
        }
        for p in &m.players {
            saved_players.insert(p.name.clone(), p.clone());
        }
        for &(x, y, secs) in &m.campfires {
            campfires.insert((x, y), secs);
        }
        for &(x, y) in &m.placed_logs {
            placed_logs.insert((x, y));
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
        start: Mutex::new(start),
        dev_token,
        saved_players: Mutex::new(saved_players),
        inventories: Mutex::new(HashMap::new()),
        respawn_points: Mutex::new(HashMap::new()),
        waypoints: Mutex::new(HashMap::new()),
        campfires: Mutex::new(campfires),
        placed_logs: Mutex::new(placed_logs),
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

/// Possibly seed spiders into a freshly generated chunk. Spiders keep to two
/// haunts: the tree-shadowed **forest** surface and the **caverns** deep
/// underground. A forest chunk that holds the grass line drops them onto the
/// ground like the other surface critters; any chunk below [`SPIDER_CAVERN_MIN_Y`]
/// drops them into a carved-out pocket with a solid floor. The per-chunk decision
/// is deterministic via [`chunk_hash`] on its own salt range, so exploring the
/// same terrain never double-spawns, and it runs independently of
/// [`maybe_spawn_in_chunk`].
fn maybe_spawn_spiders(shared: &Shared, cx: i32, cy: i32) {
    let mut world = shared.world.lock();
    let seed = world.generator.seed();
    if chunk_hash(seed, cx, cy, 100) % 100 >= SPIDER_CHUNK_CHANCE {
        return;
    }

    let base_x = cx * CHUNK_SIZE;
    let chunk_top = cy * CHUNK_SIZE;
    let chunk_bottom = chunk_top + CHUNK_SIZE;
    let count = 1 + chunk_hash(seed, cx, cy, 101) % SPIDER_CHUNK_MAX;
    let (_, sh) = EntityKind::Spider.size();

    let mut spawned = Vec::new();
    {
        let mut entities = shared.entities.lock();
        for n in 0..count {
            // Scatter spawns across the chunk's columns.
            let lx = chunk_hash(seed, cx, cy, 102 + n) % CHUNK_SIZE as u32;
            let cell_x = base_x + lx as i32;
            let surface = world.surface(cell_x);

            // Forest surface: settle a spider on the grass, but only in the chunk
            // that actually holds this column's surface line.
            let forest_surface = world.biome(cell_x) == Biome::Forest
                && surface >= chunk_top
                && surface < chunk_bottom;
            let pos = if forest_surface {
                Some((cell_x as f32 * TILE_SIZE, surface as f32 * TILE_SIZE - sh))
            } else if chunk_bottom > SPIDER_CAVERN_MIN_Y {
                // Deep underground: look for an open cavern floor in this column.
                cavern_floor_y(&mut world, cell_x, chunk_top, chunk_bottom, sh)
                    .map(|y| (cell_x as f32 * TILE_SIZE, y))
            } else {
                None
            };
            let Some((x, y)) = pos else { continue };

            let id = shared.alloc_id();
            let entity = Entity::new(id, EntityKind::Spider, x, y);
            entities.insert(entity.clone());
            spawned.push(entity);
        }
    }
    drop(world);

    for entity in spawned {
        shared.broadcast_all(ServerMessage::EntitySpawn { entity });
    }
}

/// Find a spawn y (top-left px) for a spider standing on a cavern floor within
/// column `cell_x`, searching only rows `[chunk_top, chunk_bottom)` that lie at
/// or below [`SPIDER_CAVERN_MIN_Y`]. A valid spot is an air cell with headroom
/// above and solid rock directly beneath — the floor of an open pocket — so a
/// spider never spawns embedded in stone. Returns `None` if the column's slice
/// holds no such pocket.
fn cavern_floor_y(
    world: &mut ServerWorld,
    cell_x: i32,
    chunk_top: i32,
    chunk_bottom: i32,
    sh: f32,
) -> Option<f32> {
    // Scan from the chunk's floor upward; stop once rows climb above cavern depth.
    for ty in (chunk_top..chunk_bottom).rev() {
        if ty < SPIDER_CAVERN_MIN_Y {
            break;
        }
        if !world.solid(cell_x, ty) && !world.solid(cell_x, ty - 1) && world.solid(cell_x, ty + 1) {
            // Rest the spider's feet on that floor cell.
            return Some((ty + 1) as f32 * TILE_SIZE - sh);
        }
    }
    None
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
    // Skeletons share the zombie's nightly budget, so the two undead together
    // stay capped per player rather than each filling the cap on their own.
    let undead = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Zombie | EntityKind::Skeleton))
        .count();
    if undead >= players.len() * ZOMBIE_MAX_PER_PLAYER {
        return;
    }

    // Pick a player, a side, and a distance to drop the mob at.
    next_rng(rng);
    let (px, py) = players[(*rng as usize) % players.len()];
    next_rng(rng);
    let side = if *rng & 1 == 0 { -1.0 } else { 1.0 };
    next_rng(rng);
    let span = (ZOMBIE_SPAWN_MAX_DIST - ZOMBIE_SPAWN_MIN_DIST) as u32;
    let dist = ZOMBIE_SPAWN_MIN_DIST + (*rng % span.max(1)) as f32;

    // Some of the night's undead arrive as ranged skeletons instead of zombies.
    next_rng(rng);
    let kind = if *rng % 100 < SKELETON_SPAWN_PERCENT {
        EntityKind::Skeleton
    } else {
        EntityKind::Zombie
    };
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
    let mob = Entity::new(id, kind, x, y);
    entities.insert(mob.clone());
    drop(entities);
    drop(world);
    shared.broadcast_all(ServerMessage::EntitySpawn { entity: mob });
}

/// Spawn a dropped-block item at the center of cell `(cell_x, cell_y)`, popping
/// it upward so it clears the player who mined it, and announce it to everyone.
/// Mined/crafted drops are a single item at full durability.
fn spawn_drop(shared: &Shared, cell_x: i32, cell_y: i32, block: BlockId) {
    let (iw, ih) = ITEM_SIZE;
    let x = cell_x as f32 * TILE_SIZE + (TILE_SIZE - iw) * 0.5;
    let y = cell_y as f32 * TILE_SIZE + (TILE_SIZE - ih) * 0.5;
    spawn_item(
        shared,
        block,
        1,
        crate::block::max_durability(block),
        x,
        y,
        0.0,
        ITEM_POP_VELOCITY,
        ITEM_PICKUP_DELAY,
    );
}

/// Spawn a dropped-item entity carrying its full stack and durability, with an
/// initial velocity and a pickup delay, and announce it to everyone. The
/// low-level primitive behind both mined drops and player-initiated drops.
#[allow(clippy::too_many_arguments)]
fn spawn_item(
    shared: &Shared,
    block: BlockId,
    count: u32,
    durability: u16,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    pickup_delay: f32,
) {
    if count == 0 {
        return;
    }
    let id = shared.alloc_id();
    let mut item = Entity::new(
        id,
        EntityKind::DroppedItem {
            block,
            count,
            durability,
        },
        x,
        y,
    );
    item.vx = vx;
    item.vy = vy;
    item.attack_cd = pickup_delay; // reused as the pickup-delay timer
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
    let mut since_water_flow = 0.0f32;
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
            shared.send_to(id, ServerMessage::Respawn { x, y, died: true });
        }
        // Items were credited during the step; push each collector a fresh
        // inventory snapshot (deduplicating repeat collectors).
        pickups.sort_unstable();
        pickups.dedup();
        for pid in pickups {
            shared.send_inventory(pid);
        }

        // Burn down lit campfires, extinguishing any that have run out of fuel.
        for msg in shared.tick_campfires(TICK_DT) {
            shared.broadcast_all(msg);
        }

        // Creep spreading water one cell outward through loaded terrain.
        since_water_flow += TICK_DT;
        if since_water_flow >= WATER_FLOW_SECS {
            since_water_flow = 0.0;
            for msg in shared.tick_water() {
                shared.broadcast_all(msg);
            }
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

    // Cull any non-player entity that has drifted beyond DESPAWN_DIST of every
    // player, removing it before it is simulated this tick. This keeps the world
    // from accumulating creatures and stray items in terrain nobody is near
    // (and naturally caps the always-spawning hostiles). Skipped entirely when no
    // one is connected, so a quiet world keeps its inhabitants until a player
    // returns.
    let despawns: Vec<EntityId> = if players.is_empty() {
        Vec::new()
    } else {
        entities
            .values()
            .filter(|e| !e.kind.is_player())
            .filter(|e| {
                let (w, h) = e.size();
                nearest_player(&players, e.x + w * 0.5, e.y + h * 0.5, DESPAWN_DIST).is_none()
            })
            .map(|e| e.id)
            .collect()
    };
    for &id in &despawns {
        entities.remove(id);
    }

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
    let spider_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Spider))
        .map(|e| e.id)
        .collect();
    let skeleton_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Skeleton))
        .map(|e| e.id)
        .collect();
    let bone_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| matches!(e.kind, EntityKind::Bone))
        .map(|e| e.id)
        .collect();
    let item_ids: Vec<EntityId> = entities
        .values()
        .filter(|e| e.kind.is_item())
        .map(|e| e.id)
        .collect();

    let mut broadcasts = Vec::new();
    // Tell every client to drop the entities culled for distance above.
    for id in despawns {
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }
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

    // Spiders: fast, fragile predators that scuttle after any player on sight
    // and scale sheer walls to reach them. Unlike the night-bound zombie they
    // hunt around the clock — their caverns are always pitch dark and the forest
    // canopy keeps them bold by day.
    for id in spider_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;

        let target = nearest_player(&players, scx, scy, SPIDER_AGGRO);
        let chasing = target.is_some();
        let dir = match target {
            Some((_, px, _)) if px + PLAYER_SIZE.0 * 0.5 < scx => -1.0,
            Some(_) => 1.0,
            None => wander_dir(scx, e.vx, home),
        };

        // A chasing spider climbs walls to reach its target; a wandering one
        // patrols its patch on the ground.
        let m = step_climber(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            SPIDER_SPEED,
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
                    <= SPIDER_ATTACK_RANGE
            {
                e.attack_cd = SPIDER_ATTACK_INTERVAL;
                let dir = if px + PLAYER_SIZE.0 * 0.5 >= m.x + w * 0.5 {
                    1.0
                } else {
                    -1.0
                };
                bites.push((pid, (dir * KNOCKBACK_X, -KNOCKBACK_Y), SPIDER_DAMAGE));
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

    // Skeletons: night undead archers. They stalk the player like a zombie but
    // hang back and lob bones from range. Daybreak destroys them outright —
    // unlike the zombie they have no crumble animation to play.
    let mut skeleton_despawns: Vec<EntityId> = Vec::new();
    // Bones loosed this tick, spawned after the loop so we aren't holding a
    // mutable borrow of the thrower while inserting into the same map. Each entry
    // is the bone's spawn `(x, y)` and its flight `(vx, vy)`.
    let mut throws: Vec<(f32, f32, f32, f32)> = Vec::new();
    for id in skeleton_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();

        // Daybreak: burn up and vanish on the spot (no death animation yet).
        if !night {
            skeleton_despawns.push(id);
            continue;
        }

        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);
        let home = *e.home_x.get_or_insert(e.x);
        let scx = e.x + w * 0.5;
        let scy = e.y + h * 0.5;
        let target = nearest_player(&players, scx, scy, SKELETON_AGGRO);
        let chasing = target.is_some();

        // Kiting heading: close in when out of throwing range, back off when the
        // player slips inside the standoff distance, otherwise hold and fire.
        let (dir, gap, aim) = match target {
            Some((_, px, py)) => {
                let gap = aabb_gap(e.x, e.y, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1);
                let toward = if px + PLAYER_SIZE.0 * 0.5 >= scx {
                    1.0
                } else {
                    -1.0
                };
                let dir = if gap < SKELETON_KEEP_DIST {
                    -toward // too close: retreat
                } else if gap > SKELETON_THROW_RANGE {
                    toward // too far: advance
                } else {
                    0.0 // in the sweet spot: stand and throw
                };
                (dir, gap, Some((px, py)))
            }
            None => (wander_dir(scx, e.vx, home), f32::INFINITY, None),
        };

        let m = step_ground(
            &mut world,
            (e.x, e.y, w, h),
            e.vy,
            dir,
            SKELETON_SPEED,
            chasing,
        );
        e.x = m.x;
        e.y = m.y;
        e.vx = m.vx;
        e.vy = m.vy;
        // The client derives facing from the sign of the broadcast vx. A skeleton
        // should always face the player it's fighting — even while striding
        // backwards to keep its distance — so point the reported vx's sign at the
        // target while keeping its true magnitude (so the walk cycle still plays).
        // Its real velocity stays in `e.vx` so wander heading survives losing the
        // target.
        let bcast_vx = match aim {
            Some((px, _)) => {
                let cx = m.x + w * 0.5;
                let toward = if px + PLAYER_SIZE.0 * 0.5 >= cx {
                    1.0
                } else {
                    -1.0
                };
                toward * m.vx.abs()
            }
            None => m.vx,
        };
        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: m.x,
            y: m.y,
            vx: bcast_vx,
            vy: m.vy,
        });

        // Loose a bone when a player is within range and we're off cooldown,
        // aiming from the skeleton's upper body straight at the player's center.
        if let Some((px, py)) = aim {
            if e.attack_cd <= 0.0 && gap <= SKELETON_THROW_RANGE {
                e.attack_cd = SKELETON_THROW_INTERVAL;
                let (bw, bh) = BONE_SIZE;
                let sx = m.x + w * 0.5 - bw * 0.5;
                let sy = m.y + h * 0.3 - bh * 0.5;
                let tx = px + PLAYER_SIZE.0 * 0.5;
                let ty = py + PLAYER_SIZE.1 * 0.5;
                let dx = tx - (sx + bw * 0.5);
                let dy = ty - (sy + bh * 0.5);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                throws.push((sx, sy, dx / len * BONE_SPEED, dy / len * BONE_SPEED));
            }
        }
    }
    for id in skeleton_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }
    // Spawn the bones loosed this tick. They aren't in `bone_ids`, so they begin
    // flying next tick rather than being simulated again immediately.
    for (x, y, vx, vy) in throws {
        let bid = shared.alloc_id();
        let mut bone = Entity::new(bid, EntityKind::Bone, x, y);
        bone.vx = vx;
        bone.vy = vy;
        bone.attack_cd = BONE_LIFETIME; // reused as the airborne lifetime timer
        entities.insert(bone.clone());
        broadcasts.push(ServerMessage::EntitySpawn { entity: bone });
    }

    // Bones in flight: travel in a straight line (no gravity), striking the first
    // player they overlap or winking out on a wall or when their short life ends.
    let mut bone_despawns: Vec<EntityId> = Vec::new();
    for id in bone_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        e.attack_cd = (e.attack_cd - TICK_DT).max(0.0);

        let (nx, hit_x) = move_x(&mut world, e.x, e.y, w, h, e.vx * TICK_DT);
        let (ny, hit_y) = move_y(&mut world, nx, e.y, w, h, e.vy * TICK_DT);
        e.x = nx;
        e.y = ny;

        // Struck a wall, or flew long enough without hitting anything: gone.
        if hit_x || hit_y || e.attack_cd <= 0.0 {
            bone_despawns.push(id);
            continue;
        }

        // Struck a player: deal damage, knock them along the bone's flight, gone.
        let mut struck = false;
        for &(pid, px, py) in &players {
            if aabb_gap(nx, ny, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1) <= BONE_HIT_RANGE {
                let kx = if e.vx >= 0.0 {
                    KNOCKBACK_X
                } else {
                    -KNOCKBACK_X
                };
                bites.push((pid, (kx, -KNOCKBACK_Y), BONE_DAMAGE));
                struck = true;
                break;
            }
        }
        if struck {
            bone_despawns.push(id);
            continue;
        }

        broadcasts.push(ServerMessage::EntityMoved {
            id,
            x: nx,
            y: ny,
            vx: e.vx,
            vy: e.vy,
        });
    }
    for id in bone_despawns {
        entities.remove(id);
        broadcasts.push(ServerMessage::EntityDespawn { id });
    }

    // Dropped items: fall under gravity, then get collected by any player that
    // is touching them once their pickup delay has elapsed.
    'items: for id in item_ids {
        let Some(e) = entities.get_mut(id) else {
            continue;
        };
        let (w, h) = e.size();
        let EntityKind::DroppedItem {
            block,
            count,
            durability,
        } = e.kind
        else {
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
        // A stack picks up as far as it fits; any remainder stays on the ground.
        if e.attack_cd <= 0.0 {
            for &(pid, px, py) in &players {
                if aabb_gap(x, y, w, h, px, py, PLAYER_SIZE.0, PLAYER_SIZE.1) > ITEM_PICKUP_REACH {
                    continue;
                }
                let left = shared.add_stack(pid, block, count, durability);
                if left == count {
                    continue; // no room for this player; try the next
                }
                pickups.push(pid);
                if left == 0 {
                    entities.remove(id);
                    broadcasts.push(ServerMessage::EntityDespawn { id });
                } else {
                    if let Some(e) = entities.get_mut(id)
                        && let EntityKind::DroppedItem { count: c, .. } = &mut e.kind
                    {
                        *c = left;
                    }
                    broadcasts.push(ServerMessage::EntityMoved { id, x, y, vx, vy });
                }
                continue 'items;
            }
        }
        broadcasts.push(ServerMessage::EntityMoved { id, x, y, vx, vy });
    }

    let mut respawns = Vec::new();
    for (pid, kb, damage) in bites {
        let (msgs, respawn) =
            apply_damage(&mut entities, pid, damage, kb, shared.respawn_target(pid));
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
    respawn: (f32, f32),
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
        // Death = respawn at full health at the player's respawn point (their last
        // campfire, or world spawn if they haven't used one).
        e.health = e.max_health;
        e.x = respawn.0;
        e.y = respawn.1;
        let health = e.health;
        let max_health = e.max_health;
        msgs.push(ServerMessage::EntityHealth {
            id,
            health,
            max_health,
        });
        (msgs, Some((id, respawn.0, respawn.1)))
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
    let y0 = (y / TILE_SIZE).floor() as i32;
    let y1 = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    // Substep so a fast mover can't tunnel through a tile: each pass advances at
    // most one tile and checks the column it lands in.
    let steps = (dx.abs() / TILE_SIZE).ceil().max(1.0) as i32;
    let step = dx / steps as f32;
    let mut cx = x;
    for _ in 0..steps {
        let new_x = cx + step;
        if step > 0.0 {
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
        cx = new_x;
    }
    (cx, false)
}

/// Move an AABB vertically by `dy`, stopping at the first solid row. Returns the
/// resolved y and whether the entity is now resting on the ground.
fn move_y(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, dy: f32) -> (f32, bool) {
    if dy == 0.0 {
        return (y, false);
    }
    let x0 = (x / TILE_SIZE).floor() as i32;
    let x1 = ((x + w - EPS) / TILE_SIZE).floor() as i32;
    // Substep so a fast fall can't tunnel through a tile: each pass advances at
    // most one tile and checks the row it lands in.
    let steps = (dy.abs() / TILE_SIZE).ceil().max(1.0) as i32;
    let step = dy / steps as f32;
    let mut cy = y;
    for _ in 0..steps {
        let new_y = cy + step;
        if step > 0.0 {
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
        cy = new_y;
    }
    (cy, false)
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
    // A creature already standing in water has its water-avoidance suspended, so
    // one that fell in can wade back out rather than turning back at every edge.
    let in_water = body_in_water(world, x, y, w, h);
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
    // upward), at drops too deep to step down, and at the water's edge (which they
    // prefer not to wade into). Committed ones never turn.
    if !committed
        && on_ground
        && ((hit_wall && vy >= 0.0)
            || drop_ahead(world, nx, ny, w, h, vx) >= 2
            || (!in_water && water_ahead(world, nx, ny, w, h, vx)))
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

/// Advance a wall-climbing creature (spider) one tick.
///
/// Like [`step_ground`] it walks in direction `dir` at `speed` and falls under
/// gravity, but when it runs into a wall it can *climb*: a `committed` (chasing)
/// spider that hits a wall ascends it at [`SPIDER_CLIMB_SPEED`] instead of being
/// stopped, so it scales sheer terrain to reach a player above. An uncommitted
/// (wandering) one behaves like a plain ground creature — hopping single-block
/// steps and turning back at taller walls and deep drops — so it patrols its
/// patch instead of climbing out of sight.
fn step_climber(
    world: &mut ServerWorld,
    aabb: (f32, f32, f32, f32),
    vy_in: f32,
    dir: f32,
    speed: f32,
    committed: bool,
) -> GroundMotion {
    let (x, y, w, h) = aabb;
    let grounded_before = grounded(world, x, y, w, h);
    let in_water = body_in_water(world, x, y, w, h);
    let mut vx = dir * speed;
    let mut vy = (vy_in + GRAVITY * TICK_DT).min(MAX_FALL);

    let (nx, hit_wall) = move_x(world, x, y, w, h, vx * TICK_DT);

    if hit_wall {
        if committed {
            // Chasing into a wall: climb straight up its face to follow the player.
            vy = -SPIDER_CLIMB_SPEED;
        } else if grounded_before && can_step_up(world, nx, y, w, h, vx) {
            // Wandering: clear a single-block step like a ground creature.
            vy = HOP_VELOCITY;
        }
    }

    let (ny, on_ground) = move_y(world, nx, y, w, h, vy * TICK_DT);
    if on_ground {
        vy = 0.0;
    }

    // A wandering spider reverses at a wall it didn't hop, at deep drops, and at
    // the water's edge; a chasing one never turns (it climbs walls or follows the
    // player off ledges and into water).
    if !committed
        && on_ground
        && ((hit_wall && vy >= 0.0)
            || drop_ahead(world, nx, ny, w, h, vx) >= 2
            || (!in_water && water_ahead(world, nx, ny, w, h, vx)))
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

/// Whether any cell overlapping an entity's AABB is water. Used to suspend a
/// creature's water-avoidance once it is already submerged, so it can wade back
/// out instead of jittering against the bank it can no longer escape past.
fn body_in_water(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32) -> bool {
    let x0 = (x / TILE_SIZE).floor() as i32;
    let x1 = ((x + w - EPS) / TILE_SIZE).floor() as i32;
    let y0 = (y / TILE_SIZE).floor() as i32;
    let y1 = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    (y0..=y1).any(|ty| (x0..=x1).any(|tx| crate::block::is_water(world.get(tx, ty))))
}

/// Whether water lies just ahead of a creature heading `dir`: pooled in the cell
/// it would step into at body height, or in the cell directly below that step
/// (so it won't wade off a bank into deeper water). Land creatures shy away from
/// water, turning back at its edge much as they do at a wall or a cliff.
fn water_ahead(world: &mut ServerWorld, x: f32, y: f32, w: f32, h: f32, dir: f32) -> bool {
    let ahead = if dir > 0.0 { x + w + EPS } else { x - EPS };
    let tx = (ahead / TILE_SIZE).floor() as i32;
    let y0 = (y / TILE_SIZE).floor() as i32;
    let foot = ((y + h - EPS) / TILE_SIZE).floor() as i32;
    (y0..=foot).any(|ty| crate::block::is_water(world.get(tx, ty)))
        || crate::block::is_water(world.get(tx, foot + 1))
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

    // Before anything else, check the peer speaks our wire version. Rejecting a
    // skewed client here — with a clear reason — prevents the cryptic bincode
    // "invalid variant index" errors that mismatched ClientMessage/ServerMessage
    // layouts would otherwise produce mid-session.
    let peer_version = read_version(&mut recv)
        .await
        .context("reading client protocol version")?;
    if peer_version != PROTOCOL_VERSION {
        let reason = format!(
            "protocol version mismatch: server is v{PROTOCOL_VERSION}, client is v{peer_version} — update both to the same build"
        );
        log::warn!("rejecting connection: {reason}");
        connection.close(VERSION_MISMATCH_CLOSE.into(), reason.as_bytes());
        return Ok(());
    }

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

    // Whether this connection is authorized for dev-mode commands. Set only when
    // the client presents the correct per-server dev token in `Hello`, which the
    // creator's own client is the only one to hold.
    let mut is_dev = false;

    // Reader loop.
    let read_result: Result<()> = async {
        loop {
            let msg: ClientMessage = read_msg(&mut recv).await?;
            match msg {
                ClientMessage::Hello { name, dev_token } => {
                    is_dev = dev_token == Some(shared.dev_token);
                    if is_dev {
                        log::info!("player {id} authorized for dev mode");
                    }
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
                        // Restore their last campfire respawn point, if any.
                        if let Some(rp) = sp.respawn {
                            shared.respawn_points.lock().insert(id, rp);
                        }
                        // Restore their saved inventory and push it to them.
                        shared.inventories.lock().insert(id, sp.inventory.clone());
                        shared.send_inventory(id);
                        // Restore their personal waypoints.
                        shared.waypoints.lock().insert(id, sp.waypoints.clone());
                        // Teleport the owner's avatar and resync its health. This
                        // is a reconnect, not a death, so no death marker is dropped.
                        let _ = tx.send(ServerMessage::Respawn {
                            x: sp.x,
                            y: sp.y,
                            died: false,
                        });
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
                    // Sync waypoints + home now that any saved state is restored
                    // (a fresh player just gets an empty list and the world spawn).
                    shared.send_waypoints(id);
                }
                ClientMessage::RequestChunk { cx, cy } => {
                    let (blocks, fresh) = shared.world.lock().chunk_blocks(cx, cy);
                    let _ = tx.send(ServerMessage::Chunk { cx, cy, blocks });
                    // A chunk coming into existence for the first time has a
                    // medium chance to seed creatures into its terrain.
                    if fresh {
                        maybe_spawn_in_chunk(&shared, cx, cy);
                        maybe_spawn_spiders(&shared, cx, cy);
                    }
                }
                ClientMessage::SetBlock {
                    x,
                    y,
                    block: _,
                    held,
                } => {
                    // Mining is gated by the player's melee reach — the same limit
                    // that governs attacks and placement.
                    let in_reach = {
                        let entities = shared.entities.lock();
                        entities.get(id).is_some_and(|p| {
                            let (pw, ph) = p.size();
                            aabb_gap(
                                p.x,
                                p.y,
                                pw,
                                ph,
                                x as f32 * TILE_SIZE,
                                y as f32 * TILE_SIZE,
                                TILE_SIZE,
                                TILE_SIZE,
                            ) <= PLAYER_ATTACK_REACH
                        })
                    };
                    if !in_reach {
                        // Out of range: resync the cell so the client's optimistic
                        // break is undone.
                        let actual = shared.world.lock().get(x, y);
                        shared.send_to(
                            id,
                            ServerMessage::BlockUpdate {
                                x,
                                y,
                                block: actual,
                            },
                        );
                        continue;
                    }
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
                        // A mined campfire forgets its burn timer (it drops as a
                        // plain unlit campfire via mined_drop).
                        if crate::block::is_campfire(prev) {
                            shared.campfires.lock().remove(&(x, y));
                        }
                        // Tool-gated blocks (stone, iron ore) only yield a drop
                        // when mined with a strong enough pickaxe; broken with too
                        // weak a tool they crumble to nothing. Leaves roll between
                        // a stick, an apple, or nothing.
                        if crate::block::drops_when_mined(prev, held)
                            && let Some(drop) =
                                crate::block::mined_drop_rolled(prev, shared.rand_unit())
                        {
                            spawn_drop(&shared, x, y, drop);
                        }
                        // Logs are special: breaking one updates the placed-log
                        // bookkeeping, an axe fells the whole tree, and any leaves
                        // it leaves stranded decay (still shedding their drops).
                        if prev == crate::block::LOG {
                            let was_placed = shared.placed_logs.lock().remove(&(x, y));
                            let mut removed_logs = vec![(x, y)];
                            // An axe sweeps through a naturally-grown trunk, felling
                            // every connected log at once; a player-placed log is
                            // exempt and just breaks on its own.
                            if !was_placed && crate::block::is_axe(held) {
                                for (lx, ly) in shared.chop_connected_logs(x, y) {
                                    shared.broadcast_all(ServerMessage::BlockUpdate {
                                        x: lx,
                                        y: ly,
                                        block: crate::block::AIR,
                                    });
                                    spawn_drop(&shared, lx, ly, crate::block::LOG);
                                    removed_logs.push((lx, ly));
                                }
                            }
                            for (lx, ly, drop) in shared.decay_unsupported_leaves(&removed_logs) {
                                shared.broadcast_all(ServerMessage::BlockUpdate {
                                    x: lx,
                                    y: ly,
                                    block: crate::block::AIR,
                                });
                                if let Some(drop) = drop {
                                    spawn_drop(&shared, lx, ly, drop);
                                }
                            }
                        }
                        // Breaking any part of a rope ladder reels in the whole
                        // dangling run: the cells below (and above) collapse with
                        // it. Only the cell the player struck drops an item (handled
                        // above), so a long run yields one rope ladder, not many.
                        if prev == crate::block::ROPE_LADDER {
                            for (cx, cy) in shared.collapse_rope_ladder(x, y) {
                                shared.broadcast_all(ServerMessage::BlockUpdate {
                                    x: cx,
                                    y: cy,
                                    block: crate::block::AIR,
                                });
                            }
                        }
                        // Mining wears the held tool: a pickaxe's intended job
                        // costs little, a sword or axe used to dig wears twice as fast.
                        shared.wear_tool(id, held, crate::block::mine_wear(held));
                    }
                }
                ClientMessage::PlaceBlock { x, y, slot } => {
                    // A block may only be placed into an empty cell that is
                    // orthogonally adjacent to an existing block, so players build
                    // off the world rather than dropping blocks into open air. A
                    // ladder is stricter: it mounts on the side of a wall, so it
                    // needs a solid block to the left or right (or a ladder
                    // directly above, to extend a run downward).
                    let held_slot = shared.peek_slot(id, slot as usize);
                    let placing_ladder = held_slot.is_some_and(crate::block::is_climbable);
                    // A rope ladder may additionally hang from a solid block
                    // directly above (anchored to the ground at a shaft's mouth),
                    // since it unrolls down into open air rather than clinging to a
                    // wall.
                    let placing_rope = held_slot.is_some_and(crate::block::is_rope_ladder);
                    let supported = {
                        let mut world = shared.world.lock();
                        if world.get(x, y) != crate::block::AIR {
                            false
                        } else if placing_ladder {
                            world.solid(x - 1, y)
                                || world.solid(x + 1, y)
                                || crate::block::is_climbable(world.get(x, y - 1))
                                || (placing_rope && world.solid(x, y - 1))
                        } else {
                            [(1, 0), (-1, 0), (0, 1), (0, -1)]
                                .iter()
                                .any(|(dx, dy)| world.get(x + dx, y + dy) != crate::block::AIR)
                        }
                    };
                    // Placement is also gated by the player's melee reach, so a
                    // block can only go where the player could swing — the same
                    // limit that governs attacks.
                    let in_reach = {
                        let entities = shared.entities.lock();
                        entities.get(id).is_some_and(|p| {
                            let (pw, ph) = p.size();
                            aabb_gap(
                                p.x,
                                p.y,
                                pw,
                                ph,
                                x as f32 * TILE_SIZE,
                                y as f32 * TILE_SIZE,
                                TILE_SIZE,
                                TILE_SIZE,
                            ) <= PLAYER_ATTACK_REACH
                        })
                    };
                    if !supported || !in_reach {
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
                    // Read (without removing) the block to place from the player's
                    // own slot, so they can only place what they actually hold —
                    // and so a non-placeable item (e.g. a worn tool) is never
                    // taken out and refunded, which would reset its durability.
                    match shared.peek_slot(id, slot as usize) {
                        // Plain items (bark, sticks, tools) can't be placed —
                        // leave the slot untouched and just resync it.
                        Some(block) if !shared.world.lock().registry.is_placeable(block) => {
                            shared.send_inventory(id);
                        }
                        Some(_) => {
                            // Now commit: spend the block from the slot and place it.
                            if let Some(block) = shared.take_from_slot(id, slot as usize) {
                                if block == crate::block::ROPE_LADDER {
                                    // A rope ladder unrolls downward from the target
                                    // cell, filling the shaft until it bottoms out or
                                    // its rope runs out. One placed item, many cells.
                                    let filled = shared.roll_rope_ladder(x, y);
                                    if filled.is_empty() {
                                        shared.add_item(id, block); // nothing placed: refund
                                    } else {
                                        for (fx, fy) in filled {
                                            shared.broadcast_all(ServerMessage::BlockUpdate {
                                                x: fx,
                                                y: fy,
                                                block,
                                            });
                                        }
                                    }
                                } else if shared.world.lock().set(x, y, block) {
                                    // Remember player-placed logs so an axe's
                                    // tree-felling spares what the player built.
                                    if block == crate::block::LOG {
                                        shared.placed_logs.lock().insert((x, y));
                                    }
                                    shared.broadcast_all(ServerMessage::BlockUpdate {
                                        x,
                                        y,
                                        block,
                                    });
                                } else {
                                    // Cell was occupied: refund the spent block.
                                    shared.add_item(id, block);
                                }
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
                ClientMessage::UseBucket { x, y, slot } => {
                    // Gated by the player's melee reach, the same limit governing
                    // mining and placement.
                    let in_reach = {
                        let entities = shared.entities.lock();
                        entities.get(id).is_some_and(|p| {
                            let (pw, ph) = p.size();
                            aabb_gap(
                                p.x,
                                p.y,
                                pw,
                                ph,
                                x as f32 * TILE_SIZE,
                                y as f32 * TILE_SIZE,
                                TILE_SIZE,
                                TILE_SIZE,
                            ) <= PLAYER_ATTACK_REACH
                        })
                    };
                    if in_reach {
                        shared.use_bucket(id, x, y, slot as usize);
                    } else {
                        // Out of range: resync the cell and inventory to undo the
                        // client's optimistic use.
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
                ClientMessage::MoveItem { from, to } => {
                    shared.move_item(id, from as usize, to as usize);
                    shared.send_inventory(id);
                }
                ClientMessage::DropItem { slot, all, dir } => {
                    shared.drop_item(id, slot as usize, all, dir);
                    shared.send_inventory(id);
                }
                ClientMessage::Craft { recipe } => {
                    shared.craft(id, recipe as usize);
                    shared.send_inventory(id);
                }
                ClientMessage::Smelt {
                    recipe,
                    count,
                    fuel,
                } => {
                    shared.smelt(id, recipe as usize, count, fuel);
                    shared.send_inventory(id);
                }
                ClientMessage::Repair { item } => {
                    shared.repair(id, item);
                    shared.send_inventory(id);
                }
                ClientMessage::Eat { slot } => {
                    let (msgs, respawn) = shared.eat(id, slot as usize);
                    for m in msgs {
                        shared.broadcast_all(m);
                    }
                    if let Some((rid, rx, ry)) = respawn {
                        shared.send_to(
                            rid,
                            ServerMessage::Respawn {
                                x: rx,
                                y: ry,
                                died: true,
                            },
                        );
                    }
                    shared.send_inventory(id);
                }
                ClientMessage::SetRespawn { x, y } => {
                    shared.set_respawn(id, x, y);
                    // The home waypoint follows the respawn point; resync it.
                    shared.send_waypoints(id);
                }
                ClientMessage::AddWaypoint { x, y, color } => {
                    shared.add_waypoint(id, Waypoint { x, y, color });
                }
                ClientMessage::RemoveWaypoint { x, y } => {
                    shared.remove_waypoint(id, x, y);
                }
                ClientMessage::FuelCampfire { x, y, fuel } => {
                    shared.fuel_campfire(id, x, y, fuel);
                }
                ClientMessage::Cook {
                    x,
                    y,
                    recipe,
                    count,
                } => {
                    shared.cook(id, x, y, recipe as usize, count);
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
                ClientMessage::Attack { target, held } => {
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
                        // Snapshot the victim's kind and position before the hit,
                        // so a fatal blow can spill its loot where it fell.
                        let victim = {
                            let entities = shared.entities.lock();
                            entities.get(target).map(|e| (e.kind.clone(), e.x, e.y))
                        };
                        let (msgs, respawn) = {
                            let mut entities = shared.entities.lock();
                            apply_damage(
                                &mut entities,
                                target,
                                crate::block::attack_damage(held),
                                kb,
                                shared.respawn_target(target),
                            )
                        };
                        for m in msgs {
                            shared.broadcast_all(m);
                        }
                        if let Some((rid, rx, ry)) = respawn {
                            shared.send_to(
                                rid,
                                ServerMessage::Respawn {
                                    x: rx,
                                    y: ry,
                                    died: true,
                                },
                            );
                        }
                        // If that killed a creature (it's no longer in the world),
                        // drop whatever it carries — animals leave raw meat.
                        if let Some((kind, vx, vy)) = victim
                            && shared.entities.lock().get(target).is_none()
                        {
                            let cx = (vx / TILE_SIZE) as i32;
                            let cy = (vy / TILE_SIZE) as i32;
                            for &(item, n) in creature_loot(&kind) {
                                for _ in 0..n {
                                    spawn_drop(&shared, cx, cy, item);
                                }
                            }
                        }
                        // A landed swing wears the weapon: a sword's intended job
                        // costs little, a pickaxe swung as a weapon wears double.
                        shared.wear_tool(id, held, crate::block::attack_wear(held));
                    }
                }
                ClientMessage::FallDamage { amount } => {
                    if amount > 0 {
                        let (msgs, respawn) = {
                            let mut entities = shared.entities.lock();
                            apply_damage(
                                &mut entities,
                                id,
                                amount,
                                (0.0, 0.0),
                                shared.respawn_target(id),
                            )
                        };
                        for m in msgs {
                            shared.broadcast_all(m);
                        }
                        if let Some((rid, rx, ry)) = respawn {
                            shared.send_to(
                                rid,
                                ServerMessage::Respawn {
                                    x: rx,
                                    y: ry,
                                    died: true,
                                },
                            );
                        }
                    }
                }
                ClientMessage::Chat { text } => {
                    // Attribute the line to the sender's player name (falling back
                    // to a generic label before they've identified via Hello), cap
                    // its length, and fan it out to everyone — sender included.
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        let text: String = trimmed.chars().take(MAX_CHAT_LEN).collect();
                        let from = shared
                            .entities
                            .lock()
                            .get(id)
                            .and_then(|e| match &e.kind {
                                EntityKind::Player { name } if !name.is_empty() => {
                                    Some(name.clone())
                                }
                                _ => None,
                            })
                            .unwrap_or_else(|| format!("Player {id}"));
                        shared.broadcast_all(ServerMessage::Chat { from, text });
                    }
                }
                // --- Dev-mode commands: honored only for the authorized creator.
                ClientMessage::SetTime { t } if is_dev => {
                    let t = t.rem_euclid(1.0);
                    // Rewind the clock's origin so `elapsed()` now reads `t` of a day.
                    let elapsed = Duration::from_secs_f32(t * daylight::DAY_LENGTH_SECS);
                    let new_start = Instant::now()
                        .checked_sub(elapsed)
                        .unwrap_or_else(Instant::now);
                    *shared.start.lock() = new_start;
                    shared.broadcast_all(ServerMessage::TimeOfDay { t });
                }
                ClientMessage::SpawnEntity { kind, x, y } if is_dev => {
                    // Never let dev spawn a player avatar (those are owned by a
                    // connection); only server-simulated creatures.
                    if !kind.is_player() {
                        let eid = shared.alloc_id();
                        let entity = Entity::new(eid, kind, x, y);
                        shared.entities.lock().insert(entity.clone());
                        shared.broadcast_all(ServerMessage::EntitySpawn { entity });
                    }
                }
                ClientMessage::DebugSetBlock { x, y, block } if is_dev => {
                    if shared.world.lock().set(x, y, block) {
                        shared.broadcast_all(ServerMessage::BlockUpdate { x, y, block });
                    }
                }
                // Unauthorized dev commands from a non-creator are ignored.
                ClientMessage::SetTime { .. }
                | ClientMessage::SpawnEntity { .. }
                | ClientMessage::DebugSetBlock { .. } => {
                    log::debug!("ignoring dev command from unauthorized player {id}");
                }
            }
        }
    }
    .await;

    // Cleanup. Preserve the player's state so they resume where they left off.
    shared.clients.lock().remove(&id);
    let removed = shared.entities.lock().remove(id);
    let inventory = shared.inventories.lock().remove(&id).unwrap_or_default();
    let respawn = shared.respawn_points.lock().remove(&id);
    let waypoints = shared.waypoints.lock().remove(&id).unwrap_or_default();
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
                respawn,
                waypoints,
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
