//! Game art embedded into the binary at compile time.
//!
//! Every block tile and entity animation frame is baked in with
//! [`include_bytes!`], so the executable is fully self-contained and needs no
//! `assets/` directory alongside it at runtime. The atlas loader (see
//! [`crate::client`]) decodes these PNGs directly from memory.

/// Generate [`block_png`]: a name → embedded-PNG lookup over the files in
/// `assets/textures/blocks/<name>.png`.
macro_rules! blocks {
    ($($name:literal),* $(,)?) => {
        /// Raw PNG bytes for a block texture, by block name (`None` if not embedded).
        pub fn block_png(name: &str) -> Option<&'static [u8]> {
            match name {
                $($name => Some(include_bytes!(
                    concat!("../assets/textures/blocks/", $name, ".png")
                )),)*
                _ => None,
            }
        }
    };
}

/// Generate [`sprite_png`]: a (name, frame) → embedded-PNG lookup over the files
/// in `assets/textures/entities/<name>/<frame>.png`. Each entity lists its own
/// frames, so sheets can have any number of frames (`"player" => [0, 1]`).
macro_rules! sprites {
    ($($name:literal => [$($frame:literal),+ $(,)?]),* $(,)?) => {
        /// Raw PNG bytes for one frame of an entity sprite sheet (`None` if not embedded).
        pub fn sprite_png(name: &str, frame: u32) -> Option<&'static [u8]> {
            match (name, frame) {
                $($(
                    ($name, $frame) => Some(include_bytes!(concat!(
                        "../assets/textures/entities/", $name, "/",
                        stringify!($frame), ".png"))),
                )+)*
                _ => None,
            }
        }
    };
}

/// Generate [`music_ogg`] and [`music_track_count`]: a (dimension, track) →
/// embedded-OGG lookup over the files in `assets/music/<dimension>/<track>.ogg`.
/// Each dimension lists its own track ids, so a dimension can carry any number
/// of looping tracks (`"overworld" => [0, 1, 2]`).
macro_rules! music {
    ($($dim:literal => [$($track:literal),+ $(,)?]),* $(,)?) => {
        /// Raw OGG/Vorbis bytes for one music track of a dimension (`None` if not embedded).
        pub fn music_ogg(dim: &str, track: u32) -> Option<&'static [u8]> {
            match (dim, track) {
                $($(
                    ($dim, $track) => Some(include_bytes!(concat!(
                        "../assets/music/", $dim, "/", stringify!($track), ".ogg"))),
                )+)*
                _ => None,
            }
        }

        /// How many music tracks a dimension has (`0` if the dimension is unknown).
        pub fn music_track_count(dim: &str) -> u32 {
            match dim {
                $($dim => [$($track),+].len() as u32,)*
                _ => 0,
            }
        }
    };
}

blocks!(
    "stone",
    "dirt",
    "grass",
    "log",
    "leaves",
    "wood",
    "bark",
    "stick",
    "pickaxe",
    "stone_pickaxe",
    "iron_ore",
    "raw_iron",
    "iron_ingot",
    "forge",
    "iron_pickaxe",
    "wood_sword",
    "stone_sword",
    "iron_sword",
    "ladder",
    "apple",
    "raw_meat",
    "cooked_meat",
    "campfire",
    "campfire_lit",
    "wood_axe",
    "stone_axe",
    "iron_axe",
    "rope",
    "rope_ladder",
    "coal_ore",
    "coal",
    "water",
    "bucket",
    "water_bucket",
    "charred_rock",
    "fire",
    "tungsten_ore",
    "raw_tungsten",
    "tungsten_ingot",
    "tungsten_pickaxe",
    "tungsten_sword",
    "tungsten_axe",
    "door",
    "door_top",
    "door_open",
    "door_open_top",
    "sand",
    "ash",
    "fire_key",
    "stone_bricks",
    "boat",
    "sign",
    "quest_board",
    "gold_ore",
    "raw_gold",
    "gold_ingot",
    "chest",
    "locked_chest",
    "arena_key",
    "iron_armor",
    "tungsten_armor",
    "dragon_scale",
    "summoner_spell",
    "sunburst_spell",
    "restore_spell",
    "dragonian_steed_spell",
    "paper",
);

sprites!(
    "player" => [0, 1, 2, 3, 4, 5],
    // The riding pose: the player seated in the boat, drawn while boating.
    "player/boat" => [0],
    "slime" => [0, 1, 2, 3],
    "chicken" => [0, 1, 2, 3],
    "goat" => [0, 1, 2, 3],
    "cat" => [0, 1, 2, 3],
    "cat/sit" => [0],
    "puppy" => [0, 1, 2, 3],
    "puppy/sit" => [0, 1, 2, 3, 4, 5, 6, 7],
    "horse" => [0, 1, 2, 3],
    // The riding pose: the player seated on the horse, drawn while mounted. The
    // art already includes the horse, like the boat sprite includes its rider.
    "player/horse" => [0, 1, 2, 3],
    "zombie" => [0, 1, 2, 3],
    "zombie/death" => [0, 1, 2, 3],
    "spider" => [0, 1, 2, 3],
    "snake" => [0, 1, 2, 3, 4, 5, 6, 7],
    "snake/attack" => [0, 1, 2, 3, 4, 5],
    "snake/death" => [0, 1, 2, 3, 4],
    "skeleton" => [0, 1, 2, 3, 4, 5],
    "charred_skeleton" => [0, 1, 2, 3, 4, 5],
    "demon" => [0, 1, 2, 3],
    "dragon" => [0, 1, 2, 3],
    "dragon/attack" => [0, 1],
    // The friendly white dragon the dragonian steed spell summons: a walk cycle and
    // a one-shot fire-breathing pose, mirroring the hostile dragon's sheets.
    "white_dragon" => [0, 1, 2, 3],
    "white_dragon/attack" => [0, 1],
    // The riding pose: the player seated on the white dragon, drawn while mounted.
    // The art already includes the dragon, like the player/horse sprite.
    "player/dragon" => [0, 1, 2, 3],
    "player/dragon/attack" => [0, 1],
    "demon_king" => [0, 1, 2, 3, 4],
    "demon_king/attack" => [0, 1, 2, 3, 4, 5],
    "orc" => [0, 1, 2, 3, 4],
    "orc/slam" => [0, 1, 2, 3, 4, 5],
    "ash_twister" => [0],
    "orc_mage" => [0],
    "orc_mage/cast" => [0],
    "enchanted_demon" => [0, 1, 2, 3, 4],
    "magic_fireball" => [0],
    "necromancer" => [0, 1, 2, 3],
    // The mage the restore spell conjures: a walk cycle plus a one-shot cast pose.
    "mage" => [0, 1, 2, 3],
    "mage/cast" => [0],
    "skull" => [0],
    "summoner_fireball" => [0],
    "knight" => [0, 1, 2, 3, 4],
    "knight/attack" => [0, 1, 2, 3],
    "knight/horse" => [0, 1, 2, 3],
    "knight/horse/attack" => [0, 1, 2, 3],
    "dark_knight" => [0, 1, 2, 3, 4],
    "axe" => [0, 1, 2, 3, 4, 5, 6, 7],
    "bone" => [0, 1, 2, 3],
    "fireball" => [0],
);

music!(
    "overworld" => [0, 1],
    "underworld" => [0, 1],
    "arena" => [0],
    // Played near the underworld's rare dragon miniboss (see the client's
    // proximity check), in place of the dimension's own music.
    "miniboss" => [0],
);
