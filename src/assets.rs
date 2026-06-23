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
);

sprites!(
    "player" => [0, 1, 2, 3, 4, 5],
    "slime" => [0, 1, 2, 3],
    "chicken" => [0, 1, 2, 3],
    "goat" => [0, 1, 2, 3],
    "zombie" => [0, 1, 2, 3],
    "zombie/death" => [0, 1, 2, 3],
    "spider" => [0, 1, 2, 3],
    "skeleton" => [0, 1, 2, 3, 4, 5],
    "charred_skeleton" => [0, 1, 2, 3, 4, 5],
    "bone" => [0, 1, 2, 3],
);
