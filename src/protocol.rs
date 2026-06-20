//! Wire protocol shared between client and server.
//!
//! Messages are serialized with `bincode` and sent length-prefixed (see
//! [`crate::net`]). The protocol is intentionally tiny for now; it carries
//! block ids ([`BlockId`]) as the common currency between both sides.

use serde::{Deserialize, Serialize};

use crate::entity::{Entity, EntityId};

/// Identifier of a block type. `0` is always air. See [`crate::block`].
pub type BlockId = u16;

/// ALPN protocol identifier negotiated during the QUIC/TLS handshake.
pub const ALPN: &[u8] = b"survival-cubed/0";

/// Sent from client to server over the single bidirectional stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// First message after the stream opens.
    Hello { name: String },
    /// Ask the server for the contents of a chunk.
    RequestChunk { cx: i32, cy: i32 },
    /// Place (`block != 0`) or break (`block == 0`) a block at a world cell.
    SetBlock { x: i32, y: i32, block: BlockId },
    /// Report the owning player entity's position (pixels, world space).
    PlayerMove { x: f32, y: f32 },
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
}
