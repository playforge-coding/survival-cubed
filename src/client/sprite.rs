//! Animated entity sprites.
//!
//! Every [`EntityKind`] maps to a [`SpriteDef`]: a horizontal strip of equal
//! frames loaded from `<assets>/textures/entities/<name>.png`. Like block
//! textures, a missing file is seeded from a procedural default (see
//! [`SpriteDef::default`]) so the game always has real, overwritable art on
//! disk. Frame selection is time-driven (see [`frame_index`]), so the same
//! sheet animates the player and the slimes.

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
    /// Procedural starter: RGBA texel for `(frame, x, y)`, used to write a
    /// placeholder PNG when no art exists yet.
    pub default: fn(frame: u32, x: u32, y: u32) -> [u8; 4],
}

/// Player avatar: a little humanoid whose legs stride as it walks.
pub static PLAYER_SPRITE: SpriteDef = SpriteDef {
    name: "player",
    frame_w: 16,
    frame_h: 32,
    frames: 4,
    fps: 8.0,
    default: player_tex,
};

/// Slime: a small blob that squashes and stretches as it hops along.
pub static SLIME_SPRITE: SpriteDef = SpriteDef {
    name: "slime",
    frame_w: 12,
    frame_h: 12,
    frames: 4,
    fps: 6.0,
    default: slime_tex,
};

/// Chicken: a small bird that bobs as it struts and flaps when startled.
pub static CHICKEN_SPRITE: SpriteDef = SpriteDef {
    name: "chicken",
    frame_w: 12,
    frame_h: 14,
    frames: 4,
    fps: 8.0,
    default: chicken_tex,
};

/// Every sprite the atlas needs to pack.
pub fn all() -> [&'static SpriteDef; 3] {
    [&PLAYER_SPRITE, &SLIME_SPRITE, &CHICKEN_SPRITE]
}

/// The sprite to draw for a given entity kind.
pub fn sprite_for(kind: &EntityKind) -> &'static SpriteDef {
    match kind {
        EntityKind::Player { .. } => &PLAYER_SPRITE,
        EntityKind::Slime => &SLIME_SPRITE,
        EntityKind::Chicken => &CHICKEN_SPRITE,
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

// --- Procedural starter art ----------------------------------------------

fn player_tex(frame: u32, x: u32, y: u32) -> [u8; 4] {
    const TRANS: [u8; 4] = [0, 0, 0, 0];
    const SKIN: [u8; 4] = [235, 180, 140, 255];
    const SHIRT: [u8; 4] = [60, 110, 200, 255];
    const PANTS: [u8; 4] = [45, 50, 75, 255];
    const EYE: [u8; 4] = [25, 25, 35, 255];
    let (xi, yi) = (x as i32, y as i32);

    // Head with two eyes.
    if (3..11).contains(&yi) && (4..12).contains(&xi) {
        if (6..8).contains(&yi) && (xi == 6 || xi == 9) {
            return EYE;
        }
        return SKIN;
    }
    // Torso.
    if (11..23).contains(&yi) && (3..13).contains(&xi) {
        return SHIRT;
    }
    // Legs: two columns whose stride shifts per frame to read as a walk cycle.
    if (23..32).contains(&yi) {
        let stride = [0i32, 2, 0, -2][(frame % 4) as usize];
        let left = 5 + stride;
        let right = 10 - stride;
        if (xi - left).abs() <= 1 || (xi - right).abs() <= 1 {
            return PANTS;
        }
        return TRANS;
    }
    TRANS
}

fn slime_tex(frame: u32, x: u32, y: u32) -> [u8; 4] {
    const TRANS: [u8; 4] = [0, 0, 0, 0];
    const BODY: [u8; 4] = [110, 190, 90, 255];
    const DARK: [u8; 4] = [70, 140, 60, 255];
    const EYE: [u8; 4] = [25, 30, 25, 255];

    // Squash/stretch the body ellipse per frame.
    let squash = [0.0f32, 1.0, 0.0, -1.0][(frame % 4) as usize];
    let cx = 6.0;
    let cy = 7.0 + squash * 0.5;
    let rx = 5.0 + squash;
    let ry = 4.5 - squash;
    let dx = (x as f32 + 0.5 - cx) / rx;
    let dy = (y as f32 + 0.5 - cy) / ry;
    let d = dx * dx + dy * dy;
    if d <= 1.0 {
        if y == 5 && (x == 4 || x == 8) {
            return EYE;
        }
        if d > 0.6 {
            return DARK; // rim shading
        }
        return BODY;
    }
    TRANS
}

fn chicken_tex(frame: u32, x: u32, y: u32) -> [u8; 4] {
    const TRANS: [u8; 4] = [0, 0, 0, 0];
    const BODY: [u8; 4] = [240, 240, 240, 255];
    const WING: [u8; 4] = [205, 205, 210, 255];
    const BEAK: [u8; 4] = [235, 170, 60, 255];
    const COMB: [u8; 4] = [210, 60, 55, 255];
    const LEG: [u8; 4] = [235, 170, 60, 255];
    const EYE: [u8; 4] = [25, 25, 30, 255];
    let (xi, yi) = (x as i32, y as i32);

    // Legs stride per frame to read as a walk; flap the wing on alternate frames.
    let stride = [0i32, 1, 0, -1][(frame % 4) as usize];
    let flap = frame % 2 == 1;

    // Comb on top of the head.
    if yi == 2 && (xi == 8 || xi == 9) {
        return COMB;
    }
    // Body ellipse (the plump middle of the bird).
    let dx = (xi as f32 + 0.5 - 6.0) / 5.0;
    let dy = (yi as f32 + 0.5 - 7.0) / 4.5;
    if dx * dx + dy * dy <= 1.0 {
        // Eye and beak sit toward the front (right) of the head.
        if yi == 4 && xi == 8 {
            return EYE;
        }
        if yi == 5 && xi >= 10 {
            return BEAK;
        }
        // A wing patch along the bird's flank, raised a row when flapping.
        let wing_top = if flap { 4 } else { 5 };
        if (wing_top..wing_top + 3).contains(&yi) && (3..6).contains(&xi) {
            return WING;
        }
        return BODY;
    }
    // Two legs below the body.
    if (11..14).contains(&yi) {
        let left = 5 + stride;
        let right = 7 - stride;
        if xi == left || xi == right {
            return LEG;
        }
    }
    TRANS
}
