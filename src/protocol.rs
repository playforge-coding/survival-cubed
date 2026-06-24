//! Wire protocol shared between client and server.
//!
//! Messages are serialized with `bincode` and sent length-prefixed (see
//! [`crate::net`]). The protocol is intentionally tiny for now; it carries
//! block ids ([`BlockId`]) as the common currency between both sides.

use serde::{Deserialize, Serialize};

use crate::entity::{Entity, EntityId, EntityKind};
use crate::inventory::Slot;
use crate::world::Dimension;

/// Identifier of a block type. `0` is always air. See [`crate::block`].
pub type BlockId = u16;

/// A player-placed map marker. Its world position is the player's top-left
/// (matching [`crate::entity`] coordinates), and `color` is a stable RGB chosen
/// when the waypoint is created, so the on-screen dot keeps the same hue for the
/// life of the waypoint. Default markers (home, last death) are derived on the
/// client and never travel as `Waypoint`s.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Waypoint {
    pub x: f32,
    pub y: f32,
    pub color: [f32; 3],
}

/// Wire-protocol compatibility version. **Bump this on every incompatible change
/// to anything that crosses the wire** — adding/removing/reordering a
/// [`ClientMessage`] or [`ServerMessage`] variant, changing a variant's fields,
/// or altering a transported type like [`Entity`](crate::entity::Entity) or
/// [`Slot`].
///
/// Peers exchange this as a fixed 4-byte header before any bincode (see
/// [`crate::net::read_version`]), so a version-skewed client is rejected with a
/// clear "version mismatch" message instead of the cryptic bincode
/// `invalid value: integer N, expected variant index 0 <= i < K`
/// deserialization error that a mis-aligned enum tag produces.
pub const PROTOCOL_VERSION: u32 = 15;

/// ALPN protocol identifier negotiated during the QUIC/TLS handshake. The
/// trailing number is a coarse guard bumped only for changes deep enough to
/// affect the version handshake itself; ordinary wire changes are covered by
/// [`PROTOCOL_VERSION`]. Bumping it from `/0` to `/1` here also cleanly severs
/// this build from the older `/0` binaries that predate the handshake, so they
/// can no longer connect and reproduce the bug.
pub const ALPN: &[u8] = b"survival-cubed/1";

/// Sent from client to server over the single bidirectional stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    /// First message after the stream opens. The server authenticates this
    /// before admitting the player: `name` must not already be in use by another
    /// connected player, and `password` either registers a brand-new account (on
    /// first join under this name) or must match the one stored for an existing
    /// account. A failed check closes the connection with an explanatory reason.
    ///
    /// `dev_token` is the per-server dev secret: present (and matching) only for
    /// the client that created/hosted the server, which authorizes that
    /// connection for dev-mode commands. Remote joiners send `None` and are never
    /// dev-authorized.
    Hello {
        name: String,
        password: String,
        dev_token: Option<u64>,
    },
    /// Ask the server for the contents of a chunk in dimension `dim`. The server
    /// answers from whichever dimension the player currently occupies; `dim` lets a
    /// late reply that arrives after a dimension change be discarded by the client.
    RequestChunk { dim: Dimension, cx: i32, cy: i32 },
    /// Break the block at a world cell (it drops on the ground to be collected).
    /// `held` is the item the player is wielding ([`crate::block::AIR`] for bare
    /// hands); the server uses it to decide whether the broken block drops (e.g.
    /// stone needs a pickaxe).
    SetBlock {
        x: i32,
        y: i32,
        block: BlockId,
        held: BlockId,
    },
    /// Place the block from hotbar `slot` at a world cell. The server reads the
    /// block from that slot and consumes one, so the client can't place blocks
    /// it doesn't hold.
    PlaceBlock { x: i32, y: i32, slot: u8 },
    /// Use the bucket in hotbar `slot` on world cell `(x, y)`. The server reads
    /// the slot: an empty [`bucket`](crate::block::BUCKET) scoops up a
    /// [`water`](crate::block::WATER) cell (becoming a water bucket), and a
    /// [`water_bucket`](crate::block::WATER_BUCKET) pours its water into an empty
    /// cell (becoming empty again). Validated against the player's reach.
    UseBucket { x: i32, y: i32, slot: u8 },
    /// Use the fire key held in hotbar `slot`: the server checks the slot really
    /// holds a [`fire_key`](crate::block::FIRE_KEY) and, if so, warps the player to
    /// the *other* dimension (overworld ↔ underworld), landing them at that
    /// dimension's surface in their current column. The key is reusable and is not
    /// consumed. A no-op (with a resync) if the slot no longer holds the key.
    UseFireKey { slot: u8 },
    /// Swing the door touching world cell `(x, y)` open or shut. A door spans two
    /// cells; the server flips both halves between their closed
    /// ([`crate::block::DOOR`]/[`crate::block::DOOR_TOP`]) and open
    /// ([`crate::block::DOOR_OPEN`]/[`crate::block::DOOR_OPEN_TOP`]) states.
    /// Validated against the player's reach; a no-op if `(x, y)` is not a door.
    ToggleDoor { x: i32, y: i32 },
    /// Move/merge/swap the stack in inventory slot `from` onto slot `to`.
    MoveItem { from: u8, to: u8 },
    /// Drop the contents of inventory `slot` onto the ground at the player's feet
    /// so it can be discarded or picked up by another player. `all` drops the
    /// whole stack; otherwise a single item is dropped. `dir` is the player's
    /// facing (`-1.0` left, `+1.0` right) used to toss the drop clear of them.
    /// The dropped item keeps a tool's durability.
    DropItem { slot: u8, all: bool, dir: f32 },
    /// Craft [`RECIPES`](crate::recipe::RECIPES)`[recipe]` once: the server
    /// checks the player holds all inputs, consumes them, and grants the outputs.
    Craft { recipe: u16 },
    /// Smelt [`SMELT_RECIPES`](crate::recipe::SMELT_RECIPES)`[recipe]` up to
    /// `count` times at a forge, burning `fuel` (wood, coal, or bark — see
    /// [`forge_fuel_units`](crate::block::forge_fuel_units)). The server validates
    /// the raw material plus a charge of that fuel per repetition and stops early
    /// when either runs out.
    Smelt {
        recipe: u16,
        count: u32,
        fuel: BlockId,
    },
    /// Repair one worn tool of type `item` at a forge: the server restores some
    /// durability (see [`crate::block::repair_step`]) in exchange for one unit of
    /// the tool's [`repair_material`](crate::block::repair_material).
    Repair { item: BlockId },
    /// Eat the food item in inventory `slot`: the server consumes one and adjusts
    /// the player's health by its [`food_heal`](crate::block::food_heal) amount
    /// (raw meat *costs* health). No-op if the slot doesn't hold food.
    Eat { slot: u8 },
    /// Feed one unit of `fuel` (wood, coal, or bark) to the campfire at world cell
    /// `(x, y)`, lighting it and extending its burn time. The server validates the
    /// cell is a campfire and the player holds the fuel.
    FuelCampfire { x: i32, y: i32, fuel: BlockId },
    /// Cook [`COOK_RECIPES`](crate::recipe::COOK_RECIPES)`[recipe]` up to `count`
    /// times on the campfire at world cell `(x, y)`. The server requires that
    /// campfire to be lit and validates the inputs per repetition.
    Cook {
        x: i32,
        y: i32,
        recipe: u16,
        count: u32,
    },
    /// Mark the campfire at world cell `(x, y)` as this player's respawn point, so
    /// a later death returns them here instead of world spawn. The server validates
    /// the cell is a campfire before recording it. Sent when the player opens a
    /// campfire's GUI (i.e. interacts with it).
    SetRespawn { x: i32, y: i32 },
    /// Add a personal waypoint at world pixel `(x, y)` (the player's current
    /// position), drawn with `color`. The server stores it per-player and echoes
    /// the full list back via [`ServerMessage::Waypoints`].
    AddWaypoint { x: f32, y: f32, color: [f32; 3] },
    /// Remove the personal waypoint nearest to world pixel `(x, y)`. The server
    /// resyncs the list via [`ServerMessage::Waypoints`].
    RemoveWaypoint { x: f32, y: f32 },
    /// Report the owning player entity's position (pixels, world space).
    PlayerMove { x: f32, y: f32 },
    /// Melee-attack another entity (e.g. a slime). The server validates range
    /// before applying damage. `held` is the item the player is wielding
    /// ([`crate::block::AIR`] for bare hands); the server uses it to scale the
    /// damage (swords hit hardest, pickaxes far less). See
    /// [`crate::block::attack_damage`].
    Attack { target: EntityId, held: BlockId },
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
    /// Debug: drop `count` of item `item` straight into the dev's inventory (the
    /// item-giver UI). The server validates `item` is a real id and stacks it in,
    /// then resyncs the inventory.
    GiveItem { item: BlockId, count: u32 },
    /// Send a line of chat. The server attributes it to this connection's
    /// player name and rebroadcasts it to everyone (see [`ServerMessage::Chat`]).
    Chat { text: String },
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
    /// Full contents of a chunk (row-major, `CHUNK_AREA` entries) in dimension
    /// `dim`. The client ignores chunks for a dimension it is no longer in.
    Chunk {
        dim: Dimension,
        cx: i32,
        cy: i32,
        blocks: Vec<BlockId>,
    },
    /// A single block changed somewhere in dimension `dim`. The client ignores
    /// updates for a dimension it is not currently in.
    BlockUpdate {
        dim: Dimension,
        x: i32,
        y: i32,
        block: BlockId,
    },
    /// Move the owning client into dimension `dim` at world pixel `(x, y)`: it
    /// clears its mirrored world and entities, switches dimension, and repositions
    /// its avatar. Sent when the player falls into the underworld or climbs back to
    /// the overworld (see [`crate::server`]'s dimension transitions).
    EnterDimension { dim: Dimension, x: f32, y: f32 },
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
    /// A snake has begun a telegraphed wind-up lunge. Every client plays its
    /// strike animation for [`crate::entity::SNAKE_LUNGE_TIME`] seconds; the
    /// snake's forward spring and bite arrive as ordinary
    /// [`ServerMessage::EntityMoved`]/[`ServerMessage::EntityHit`] updates.
    EntityLunging { id: EntityId },
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
    /// point. Health is restored via a separate `EntityHealth`. `died` is `true`
    /// when this is a death respawn (the client drops a "last death" waypoint at
    /// the spot it was standing) and `false` for a reconnect teleport to the
    /// player's saved position.
    Respawn { x: f32, y: f32, died: bool },
    /// Authoritative snapshot of the owning player's personal waypoints plus the
    /// current home (respawn) point in world pixels. Sent on join and after any
    /// waypoint or respawn-point change. Only ever sent to the list's owner.
    Waypoints {
        list: Vec<Waypoint>,
        home: (f32, f32),
    },
    /// Authoritative snapshot of the owning player's inventory slots (hotbar
    /// first, then storage). Sent on join and after any change (pickup,
    /// placement, slot move). Only ever sent to the inventory's owner.
    Inventory { slots: Vec<Slot> },
    /// A chat line to display, attributed to player `from`. Broadcast to every
    /// client (including the original sender, so they see their own message).
    /// Admin command feedback and ban announcements arrive on this same channel,
    /// attributed to a `Server` pseudo-sender.
    Chat { from: String, text: String },
    /// Begin or end spectating another player. `Some(id)` locks the receiving
    /// (admin) client's camera onto the entity with that id — which the server has
    /// already moved the admin alongside so it streams in — and freezes the admin's
    /// own avatar; `None` releases the camera back to the admin's avatar. Only ever
    /// sent to an admin who issued `/spectate`. See [`crate::server`].
    Spectate { target: Option<EntityId> },
}
