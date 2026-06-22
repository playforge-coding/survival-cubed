//! Animated entity sprites.
//!
//! Every [`EntityKind`] maps to a [`SpriteDef`]: a strip of equal frames, one
//! PNG per frame, baked into the binary (see [`crate::assets`]) and decoded by
//! the atlas loader. Frame selection is time-driven (see [`frame_index`]), so
//! the same sheet animates the player and the slimes.

use crate::entity::EntityKind;

/// Describes one entity's animation sheet.
pub struct SpriteDef {
    /// File stem under the entities texture dir, and atlas lookup key.
    pub name: &'static str,
    /// Width of a single frame, in texels.
    pub frame_w: u32,
    /// Height of a single frame, in texels.
    pub frame_h: u32,
    /// Number of frames laid out left-to-right in the sheet.
    pub frames: u32,
    /// Animation playback speed, in frames per second.
    pub fps: f32,
}

/// Player avatar: a little humanoid whose legs stride as it walks.
pub static PLAYER_SPRITE: SpriteDef = SpriteDef {
    name: "player",
    frame_w: 11,
    frame_h: 16,
    frames: 6,
    fps: 8.0,
};

/// Slime: a small blob that squashes and stretches as it hops along.
pub static SLIME_SPRITE: SpriteDef = SpriteDef {
    name: "slime",
    frame_w: 12,
    frame_h: 12,
    frames: 4,
    fps: 6.0,
};

/// Chicken: a small bird that bobs as it struts and flaps when startled.
pub static CHICKEN_SPRITE: SpriteDef = SpriteDef {
    name: "chicken",
    frame_w: 12,
    frame_h: 14,
    frames: 4,
    fps: 8.0,
};

/// Goat: a stocky mountain grazer that ambles along on four legs.
pub static GOAT_SPRITE: SpriteDef = SpriteDef {
    name: "goat",
    frame_w: 16,
    frame_h: 16,
    frames: 4,
    fps: 6.0,
};

/// Zombie: a shambling undead that lurches along, arms out.
pub static ZOMBIE_SPRITE: SpriteDef = SpriteDef {
    name: "zombie",
    frame_w: 14,
    frame_h: 19,
    frames: 4,
    fps: 4.0,
};

/// Zombie death: a one-shot crumble played as the undead burns up in daylight.
/// Lives in the `zombie/death` subdirectory (its `name` doubles as that path),
/// and its frames are stepped by the death timer rather than the walk clock.
pub static ZOMBIE_DEATH_SPRITE: SpriteDef = SpriteDef {
    name: "zombie/death",
    frame_w: 12,
    frame_h: 19,
    frames: 4,
    fps: 6.0,
};

/// Spider: a low, many-legged scuttler whose legs ripple as it darts and climbs.
pub static SPIDER_SPRITE: SpriteDef = SpriteDef {
    name: "spider",
    frame_w: 16,
    frame_h: 16,
    frames: 4,
    fps: 10.0,
};

/// Every sprite the atlas needs to pack.
pub fn all() -> [&'static SpriteDef; 7] {
    [
        &PLAYER_SPRITE,
        &SLIME_SPRITE,
        &CHICKEN_SPRITE,
        &GOAT_SPRITE,
        &ZOMBIE_SPRITE,
        &ZOMBIE_DEATH_SPRITE,
        &SPIDER_SPRITE,
    ]
}

/// The sprite to draw for a given entity kind.
pub fn sprite_for(kind: &EntityKind) -> &'static SpriteDef {
    match kind {
        EntityKind::Player { .. } => &PLAYER_SPRITE,
        EntityKind::Slime => &SLIME_SPRITE,
        EntityKind::Chicken => &CHICKEN_SPRITE,
        EntityKind::Goat => &GOAT_SPRITE,
        EntityKind::Zombie => &ZOMBIE_SPRITE,
        EntityKind::Spider => &SPIDER_SPRITE,
        // Dropped items are drawn from their block texture, not an animation
        // sheet (see the client's scene builder), so this is never queried for
        // them; fall back to the slime sheet to keep the match total.
        EntityKind::DroppedItem { .. } => &SLIME_SPRITE,
    }
}

/// Pick a frame for `clock` seconds of elapsed time. A still entity shows the
/// resting frame (0); a moving one cycles through the sheet.
pub fn frame_index(moving: bool, clock: f32, def: &SpriteDef) -> u32 {
    if def.frames <= 1 || !moving {
        0
    } else {
        ((clock * def.fps) as u32) % def.frames
    }
}
