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

/// Boat (with rider): the player seated in a boat, drawn in place of the plain
/// player sprite while boating. A single static frame; the art already includes
/// the rider. Lives in the `player/boat` subdirectory (its `name` is that path).
pub static BOAT_SPRITE: SpriteDef = SpriteDef {
    name: "player/boat",
    frame_w: 16,
    frame_h: 20,
    frames: 1,
    fps: 1.0,
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

/// Cat: a small forest critter that pads along, tail swaying, when it moves.
pub static CAT_SPRITE: SpriteDef = SpriteDef {
    name: "cat",
    frame_w: 15,
    frame_h: 13,
    frames: 4,
    fps: 8.0,
};

/// Cat (sitting): a one-frame resting pose shown while a cat has been told to sit.
/// Lives in the `cat/sit` subdirectory (its `name` doubles as that path).
pub static CAT_SIT_SPRITE: SpriteDef = SpriteDef {
    name: "cat/sit",
    frame_w: 15,
    frame_h: 13,
    frames: 1,
    fps: 1.0,
};

/// Puppy: a small forest critter that trots along on its four-frame walk cycle.
pub static PUPPY_SPRITE: SpriteDef = SpriteDef {
    name: "puppy",
    frame_w: 20,
    frame_h: 14,
    frames: 4,
    fps: 8.0,
};

/// Puppy (sitting): an eight-frame looping idle played while a puppy has been told
/// to sit (it breathes/looks around in place). Lives in the `puppy/sit`
/// subdirectory (its `name` doubles as that path). Unlike a walk sheet it loops on
/// the shared clock even while the puppy is stationary (see the scene builder).
pub static PUPPY_SIT_SPRITE: SpriteDef = SpriteDef {
    name: "puppy/sit",
    frame_w: 18,
    frame_h: 14,
    frames: 8,
    fps: 6.0,
};

/// Horse (riderless): a tall plains grazer that ambles along on its four-frame
/// walk cycle. The wild/unmounted horse; a ridden one is drawn with
/// [`PLAYER_HORSE_SPRITE`] instead.
pub static HORSE_SPRITE: SpriteDef = SpriteDef {
    name: "horse",
    frame_w: 17,
    frame_h: 14,
    frames: 4,
    fps: 8.0,
};

/// Horse (with rider): the player seated on a horse, drawn in place of the plain
/// player sprite while mounted — a four-frame gallop whose art already includes the
/// horse, just as [`BOAT_SPRITE`] already includes its rider. Lives in the
/// `player/horse` subdirectory (its `name` is that path).
pub static PLAYER_HORSE_SPRITE: SpriteDef = SpriteDef {
    name: "player/horse",
    frame_w: 18,
    frame_h: 21,
    frames: 4,
    fps: 8.0,
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

/// Snake: a low desert ambusher that slinks along the sand, body rippling.
pub static SNAKE_SPRITE: SpriteDef = SpriteDef {
    name: "snake",
    frame_w: 16,
    frame_h: 14,
    frames: 8,
    fps: 10.0,
};

/// Snake (striking): the one-shot wind-up lunge — the snake coils back and
/// springs. Lives in the `snake/attack` subdirectory (its `name` doubles as that
/// path), and its frames are stepped by the lunge timer rather than the walk clock.
pub static SNAKE_ATTACK_SPRITE: SpriteDef = SpriteDef {
    name: "snake/attack",
    frame_w: 16,
    frame_h: 14,
    frames: 6,
    fps: 8.0,
};

/// Snake death: a one-shot writhe played as the snake is killed. Lives in the
/// `snake/death` subdirectory (its `name` doubles as that path), and its frames
/// are stepped by the death timer rather than the walk clock.
pub static SNAKE_DEATH_SPRITE: SpriteDef = SpriteDef {
    name: "snake/death",
    frame_w: 16,
    frame_h: 14,
    frames: 5,
    fps: 8.0,
};

/// Skeleton: a lanky undead archer that strides along as it stalks the player.
pub static SKELETON_SPRITE: SpriteDef = SpriteDef {
    name: "skeleton",
    frame_w: 11,
    frame_h: 16,
    frames: 6,
    fps: 8.0,
};

/// Charred skeleton: a scorched undead that charges along on the same lanky build
/// as the surface skeleton, trailing fire as it hunts.
pub static CHARRED_SKELETON_SPRITE: SpriteDef = SpriteDef {
    name: "charred_skeleton",
    frame_w: 11,
    frame_h: 16,
    frames: 6,
    fps: 8.0,
};

/// Demon: a hunched underworld fiend that flits along as it stalks the player,
/// hurling fireballs from range.
pub static DEMON_SPRITE: SpriteDef = SpriteDef {
    name: "demon",
    frame_w: 10,
    frame_h: 15,
    frames: 4,
    fps: 8.0,
};

/// Orc: a hulking underworld brute that lumbers along on a slow, heavy stride.
pub static ORC_SPRITE: SpriteDef = SpriteDef {
    name: "orc",
    frame_w: 10,
    frame_h: 15,
    frames: 5,
    fps: 5.0,
};

/// Orc slam: the one-shot telegraphed attack — the orc heaves its arms up and
/// crashes them down. Lives in the `orc/slam` subdirectory (its `name` doubles as
/// that path), and its frames are stepped by the slam (lunge) timer rather than the
/// walk clock. The blow lands on frame 3, where the fists hit the ground.
pub static ORC_SLAM_SPRITE: SpriteDef = SpriteDef {
    name: "orc/slam",
    frame_w: 12,
    frame_h: 15,
    frames: 6,
    fps: 6.0,
};

/// Bone: a small thrown projectile that tumbles end over end as it flies.
pub static BONE_SPRITE: SpriteDef = SpriteDef {
    name: "bone",
    frame_w: 16,
    frame_h: 16,
    frames: 4,
    fps: 12.0,
};

/// Fireball: a small bolt of flame a demon hurls, a single glowing frame that
/// flies until it bursts.
pub static FIREBALL_SPRITE: SpriteDef = SpriteDef {
    name: "fireball",
    frame_w: 10,
    frame_h: 7,
    frames: 1,
    fps: 1.0,
};

/// Every sprite the atlas needs to pack.
pub fn all() -> [&'static SpriteDef; 24] {
    [
        &PLAYER_SPRITE,
        &BOAT_SPRITE,
        &SLIME_SPRITE,
        &CHICKEN_SPRITE,
        &GOAT_SPRITE,
        &CAT_SPRITE,
        &CAT_SIT_SPRITE,
        &PUPPY_SPRITE,
        &PUPPY_SIT_SPRITE,
        &HORSE_SPRITE,
        &PLAYER_HORSE_SPRITE,
        &ZOMBIE_SPRITE,
        &ZOMBIE_DEATH_SPRITE,
        &SPIDER_SPRITE,
        &SNAKE_SPRITE,
        &SNAKE_ATTACK_SPRITE,
        &SNAKE_DEATH_SPRITE,
        &SKELETON_SPRITE,
        &CHARRED_SKELETON_SPRITE,
        &DEMON_SPRITE,
        &ORC_SPRITE,
        &ORC_SLAM_SPRITE,
        &BONE_SPRITE,
        &FIREBALL_SPRITE,
    ]
}

/// The sprite to draw for a given entity kind.
pub fn sprite_for(kind: &EntityKind) -> &'static SpriteDef {
    match kind {
        EntityKind::Player { .. } => &PLAYER_SPRITE,
        EntityKind::Slime => &SLIME_SPRITE,
        EntityKind::Chicken => &CHICKEN_SPRITE,
        EntityKind::Goat => &GOAT_SPRITE,
        // A sitting cat shows its one-frame resting pose; otherwise the walk sheet.
        EntityKind::Cat { sitting: true, .. } => &CAT_SIT_SPRITE,
        EntityKind::Cat { .. } => &CAT_SPRITE,
        // A sitting puppy shows its one-frame resting pose; otherwise the walk sheet.
        EntityKind::Puppy { sitting: true, .. } => &PUPPY_SIT_SPRITE,
        EntityKind::Puppy { .. } => &PUPPY_SPRITE,
        // A ridden horse is drawn as the combined player/horse sprite by the scene
        // builder; this riderless sheet is the wild/unmounted horse.
        EntityKind::Horse { .. } => &HORSE_SPRITE,
        EntityKind::Zombie => &ZOMBIE_SPRITE,
        EntityKind::Spider => &SPIDER_SPRITE,
        // A snake's striking pose is handled by the scene builder off its lunge
        // timer; this walk sheet is its resting/slithering animation.
        EntityKind::Snake => &SNAKE_SPRITE,
        EntityKind::Skeleton => &SKELETON_SPRITE,
        EntityKind::CharredSkeleton => &CHARRED_SKELETON_SPRITE,
        EntityKind::Demon => &DEMON_SPRITE,
        // An orc's slam pose is handled by the scene builder off its lunge timer;
        // this walk sheet is its plodding stride.
        EntityKind::Orc => &ORC_SPRITE,
        EntityKind::Bone => &BONE_SPRITE,
        EntityKind::Fireball => &FIREBALL_SPRITE,
        // Dropped items are drawn from their block texture, not an animation
        // sheet (see the client's scene builder), so this is never queried for
        // them; fall back to the slime sheet to keep the match total.
        EntityKind::DroppedItem { .. } => &SLIME_SPRITE,
    }
}

/// The death-animation sheet to play for a kind that has one (stepped by the
/// entity's death timer, not the walk clock), or `None` if it simply vanishes
/// when it dies. Pairs with [`EntityKind::death_time`] for the playback duration.
pub fn death_sprite_for(kind: &EntityKind) -> Option<&'static SpriteDef> {
    match kind {
        EntityKind::Zombie => Some(&ZOMBIE_DEATH_SPRITE),
        EntityKind::Snake => Some(&SNAKE_DEATH_SPRITE),
        _ => None,
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
