//! Entities: anything that lives in the world but isn't a block.
//!
//! Blocks are static cells on the world grid; entities are free-moving objects
//! addressed by a unique [`EntityId`] and positioned in pixel/world space. Both
//! client and server share these types so an entity can be described once and
//! sent over the wire (see [`crate::protocol`]).
//!
//! The player is "just" an entity — see [`EntityKind::Player`] — but a *special*
//! one: its position is authoritative from the client that owns it and the
//! server never runs AI on it. Every other kind (e.g. [`EntityKind::Slime`])
//! is simulated by the server's tick loop. That distinction is the whole point
//! of [`EntityKind::is_player`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::protocol::BlockId;

/// Unique identifier of a live entity. Allocated by the server; `0` is never
/// used so it can double as "no entity".
pub type EntityId = u32;

/// Collision/draw size (width, height) in pixels of a player avatar.
pub const PLAYER_SIZE: (f32, f32) = (16.0, 32.0);
/// Collision/draw size (width, height) in pixels of a slime.
pub const SLIME_SIZE: (f32, f32) = (12.0, 12.0);
/// Collision/draw size (width, height) in pixels of a chicken.
pub const CHICKEN_SIZE: (f32, f32) = (12.0, 14.0);
/// Collision/draw size (width, height) in pixels of a dropped block item.
pub const ITEM_SIZE: (f32, f32) = (8.0, 8.0);

/// Maximum health of a player, in hit points.
pub const PLAYER_MAX_HEALTH: i32 = 20;
/// Maximum health of a slime, in hit points.
pub const SLIME_MAX_HEALTH: i32 = 10;
/// Maximum health of a chicken, in hit points.
pub const CHICKEN_MAX_HEALTH: i32 = 8;

/// What an entity *is*. Adding a new creature/object means adding a variant
/// here plus (for server-simulated kinds) a branch in the server tick loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EntityKind {
    /// A player avatar driven by a connected client. Special: client-authoritative
    /// position, never touched by server AI. Carries the player's display name.
    Player { name: String },
    /// A small creature that wanders the surface. Server-simulated.
    Slime,
    /// A harmless bird that pecks around the surface and bolts away from a
    /// player that hits it. Server-simulated.
    Chicken,
    /// A block lying on the ground after being mined, waiting to be walked into
    /// and picked up. Server-simulated (falls under gravity); carries the block
    /// id it will add to a player's inventory on pickup.
    DroppedItem { block: BlockId },
}

impl EntityKind {
    /// Draw/collision size (width, height) in pixels for this kind.
    pub fn size(&self) -> (f32, f32) {
        match self {
            EntityKind::Player { .. } => PLAYER_SIZE,
            EntityKind::Slime => SLIME_SIZE,
            EntityKind::Chicken => CHICKEN_SIZE,
            EntityKind::DroppedItem { .. } => ITEM_SIZE,
        }
    }

    /// Whether this is a player avatar (the "special" entity the owning client
    /// simulates itself).
    pub fn is_player(&self) -> bool {
        matches!(self, EntityKind::Player { .. })
    }

    /// Whether this is a dropped block item lying on the ground.
    pub fn is_item(&self) -> bool {
        matches!(self, EntityKind::DroppedItem { .. })
    }

    /// Full health for this kind of entity. Players cap at
    /// [`PLAYER_MAX_HEALTH`]; other creatures have their own (see the
    /// constants above).
    pub fn max_health(&self) -> i32 {
        match self {
            EntityKind::Player { .. } => PLAYER_MAX_HEALTH,
            EntityKind::Slime => SLIME_MAX_HEALTH,
            EntityKind::Chicken => CHICKEN_MAX_HEALTH,
            // Items are inert; 1 keeps health == max_health so no health bar shows.
            EntityKind::DroppedItem { .. } => 1,
        }
    }
}

/// A live entity: its identity, kind, and current motion state. Position is the
/// top-left corner in world pixels, matching how the player and tiles are drawn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub kind: EntityKind,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    /// Current health in hit points. Starts at [`EntityKind::max_health`].
    pub health: i32,
    /// Full health for this entity, mirrored from its kind for convenience on
    /// the client (so health bars know their denominator without a registry).
    pub max_health: i32,
    /// Server-only: seconds until this creature can attack again. Never sent
    /// over the wire (defaults to `0.0` on the client).
    #[serde(skip)]
    pub attack_cd: f32,
    /// Server-only: seconds a skittish creature (e.g. a [`EntityKind::Chicken`])
    /// keeps fleeing after being hit. Counts down each tick; while positive the
    /// creature runs from the nearest player. Never sent over the wire.
    #[serde(skip)]
    pub flee: f32,
    /// Server-only: the x (world px) a wandering creature treats as the center of
    /// its home range, so it loiters nearby instead of drifting off forever. Set
    /// lazily to wherever the creature first simulates (`None` until then), so it
    /// survives a reload without needing to be persisted.
    #[serde(skip)]
    pub home_x: Option<f32>,
}

impl Entity {
    /// Create an entity at rest and at full health at `(x, y)`.
    pub fn new(id: EntityId, kind: EntityKind, x: f32, y: f32) -> Self {
        let max_health = kind.max_health();
        Entity {
            id,
            kind,
            x,
            y,
            vx: 0.0,
            vy: 0.0,
            health: max_health,
            max_health,
            attack_cd: 0.0,
            flee: 0.0,
            home_x: None,
        }
    }

    /// Draw/collision size (width, height) in pixels.
    pub fn size(&self) -> (f32, f32) {
        self.kind.size()
    }
}

/// A live collection of entities keyed by id. Used by the server (the
/// authority) and mirrored on each client for everything *except* its own
/// player avatar, which the client simulates locally.
#[derive(Default)]
pub struct Entities {
    map: HashMap<EntityId, Entity>,
}

impl Entities {
    pub fn new() -> Self {
        Entities {
            map: HashMap::new(),
        }
    }

    pub fn insert(&mut self, entity: Entity) {
        self.map.insert(entity.id, entity);
    }

    pub fn remove(&mut self, id: EntityId) -> Option<Entity> {
        self.map.remove(&id)
    }

    pub fn get(&self, id: EntityId) -> Option<&Entity> {
        self.map.get(&id)
    }

    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut Entity> {
        self.map.get_mut(&id)
    }

    pub fn values(&self) -> impl Iterator<Item = &Entity> {
        self.map.values()
    }

    /// Number of player entities currently present.
    pub fn player_count(&self) -> usize {
        self.map.values().filter(|e| e.kind.is_player()).count()
    }
}
