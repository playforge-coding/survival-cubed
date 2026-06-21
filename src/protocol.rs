//! Wire protocol shared between client and server.
//!
//! Messages are serialized with `bincode` and sent length-prefixed (see
//! [`crate::net`]). The protocol is intentionally tiny for now; it carries
//! block ids ([`BlockId`]) as the common currency between both sides.

use serde::{Deserialize, Serialize};

use crate::entity::{Entity, EntityId, EntityKind};
use crate::inventory::Slot;

/// Identifier of a block type. `0` is always air. See [`crate::block`].
pub type BlockId = u16;

/// ALPN protocol identifier negotiated during the QUIC/TLS handshake.
pub const ALPN: &[u8] = b"survival-cubed/0";

/// Sent from client to server over the single bidirectional stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// First message after the stream opens. `dev_token` is the per-server dev
    /// secret: present (and matching) only for the client that created/hosted the
    /// server, which authorizes that connection for dev-mode commands. Remote
    /// joiners send `None` and are never dev-authorized.
    Hello {
        name: String,
        dev_token: Option<u64>,
    },
    /// Ask the server for the contents of a chunk.
    RequestChunk { cx: i32, cy: i32 },
    /// Break the block at a world cell (it drops on the ground to be collected).
    SetBlock { x: i32, y: i32, block: BlockId },
    /// Place the block from hotbar `slot` at a world cell. The server reads the
    /// block from that slot and consumes one, so the client can't place blocks
    /// it doesn't hold.
    PlaceBlock { x: i32, y: i32, slot: u8 },
    /// Move/merge/swap the stack in inventory slot `from` onto slot `to`.
    MoveItem { from: u8, to: u8 },
    /// Report the owning player entity's position (pixels, world space).
    PlayerMove { x: f32, y: f32 },
    /// Melee-attack another entity (e.g. a slime). The server validates range
    /// before applying damage.
    Attack { target: EntityId },
    /// Report fall damage the client computed from its own landing. The server
    /// is authoritative over the resulting health.
    FallDamage { amount: i32 },
    /// Debug: jump the world clock to normalized time of day `t` in `[0, 1)`.
    /// The server adjusts its authoritative clock and rebroadcasts the time.
    SetTime { t: f32 },
    /// Debug: spawn a creature of `kind` at world pixel `(x, y)`. Player kinds
    /// are ignored by the server.
    SpawnEntity { kind: EntityKind, x: f32, y: f32 },
    /// Debug: set the block at a world cell directly, with no inventory cost or
    /// adjacency requirement (used by dev mode's infinite-block placement).
    DebugSetBlock { x: i32, y: i32, block: BlockId },
}

/// Sent from server to client over the single bidirectional stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Response to `Hello`: identifies the client's own player entity and its
    /// spawn position (pixels).
    Welcome {
        entity_id: EntityId,
        spawn_x: f32,
        spawn_y: f32,
    },
    /// Full contents of a chunk (row-major, `CHUNK_AREA` entries).
    Chunk {
        cx: i32,
        cy: i32,
        blocks: Vec<BlockId>,
    },
    /// A single block changed somewhere in the world.
    BlockUpdate { x: i32, y: i32, block: BlockId },
    /// An entity appeared (or its full description is being (re)sent). The
    /// client never receives a spawn for its own player entity.
    EntitySpawn { entity: Entity },
    /// Lightweight position/velocity update for an existing entity.
    EntityMoved {
        id: EntityId,
        x: f32,
        y: f32,
        vx: f32,
        vy: f32,
    },
    /// An entity was removed from the world.
    EntityDespawn { id: EntityId },
    /// A zombie has been caught by daylight and begun its death animation. The
    /// client plays the crumble animation for [`crate::entity::ZOMBIE_DEATH_TIME`]
    /// seconds; an [`ServerMessage::EntityDespawn`] for the same id follows once
    /// it finishes.
    EntityDying { id: EntityId },
    /// An entity's health changed (damage, healing, or an initial value). Sent
    /// to every client, including the owner of a player entity (whose avatar is
    /// otherwise never mirrored).
    EntityHealth {
        id: EntityId,
        health: i32,
        max_health: i32,
    },
    /// An entity just took a hit. Every client flashes that entity red; the
    /// owning client of a player avatar also applies the knockback velocity
    /// `(vx, vy)` (px/s) to its locally-simulated motion. Server-simulated
    /// creatures are already knocked back on the server, so for them the
    /// velocity is informational only.
    EntityHit { id: EntityId, vx: f32, vy: f32 },
    /// Current normalized time of day in `[0, 1)` (see [`crate::daylight`]).
    /// Broadcast periodically; clients advance it locally in between.
    TimeOfDay { t: f32 },
    /// Instruct the owning client to move its player avatar back to a spawn
    /// point (after death). Health is restored via a separate `EntityHealth`.
    Respawn { x: f32, y: f32 },
    /// Authoritative snapshot of the owning player's inventory slots (hotbar
    /// first, then storage). Sent on join and after any change (pickup,
    /// placement, slot move). Only ever sent to the inventory's owner.
    Inventory { slots: Vec<Slot> },
}
