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
/// in `assets/textures/entities/<name>/<frame>.png`. Every entity sheet has four
/// frames (`0.png`..`3.png`).
macro_rules! sprites {
    ($($name:literal),* $(,)?) => {
        /// Raw PNG bytes for one frame of an entity sprite sheet (`None` if not embedded).
        pub fn sprite_png(name: &str, frame: u32) -> Option<&'static [u8]> {
            match (name, frame) {
                $(
                    ($name, 0) => Some(include_bytes!(
                        concat!("../assets/textures/entities/", $name, "/0.png"))),
                    ($name, 1) => Some(include_bytes!(
                        concat!("../assets/textures/entities/", $name, "/1.png"))),
                    ($name, 2) => Some(include_bytes!(
                        concat!("../assets/textures/entities/", $name, "/2.png"))),
                    ($name, 3) => Some(include_bytes!(
                        concat!("../assets/textures/entities/", $name, "/3.png"))),
                )*
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
);

sprites!(
    "player",
    "slime",
    "chicken",
    "goat",
    "zombie",
    "zombie/death"
);
