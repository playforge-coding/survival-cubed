//! Block definitions and an extensible registry.
//!
//! Blocks are data-driven: a [`BlockDef`] describes a block's name, whether it
//! is solid, and where to find its 16x16 sprite. New blocks are added by
//! calling [`BlockRegistry::register`] — nothing else in the engine needs to
//! change. A block's id is its index in the registry (and its cell in the
//! texture atlas), so registration order defines ids.
//!
//! Sprites are loaded at runtime from PNG files (see [`crate::client`]'s atlas
//! loader). Each visible block looks for `<name>.png` in the textures
//! directory. If the file is missing, a starter image is generated from the
//! block's [`default_tex`](BlockDef::default_tex) so the game still runs and
//! you have a template to overwrite.

use crate::protocol::BlockId;

/// Pixel size of one block sprite, in texels.
pub const TILE_TEX: u32 = 16;

/// Built-in block ids. These are guaranteed because [`BlockRegistry::new`]
/// registers them first, in this order.
pub const AIR: BlockId = 0;
pub const STONE: BlockId = 1;
pub const DIRT: BlockId = 2;
pub const GRASS: BlockId = 3;
pub const LOG: BlockId = 4;
pub const LEAVES: BlockId = 5;

/// Generates an RGBA texel at `(x, y)` (both in `0..TILE_TEX`). Used only to
/// seed a starter PNG when the real texture file is missing.
pub type TexFn = fn(x: u32, y: u32) -> [u8; 4];

/// Definition of a single block type.
pub struct BlockDef {
    pub id: BlockId,
    pub name: &'static str,
    /// Whether the player collides with this block.
    pub solid: bool,
    /// Whether the block is drawn (air is not).
    pub visible: bool,
    /// Seconds of sustained mining needed to break this block (the breaking
    /// delay). Tougher blocks take longer; air is `0.0`.
    pub break_secs: f32,
    /// Optional procedural fallback used to write a starter `<name>.png` when
    /// no texture file exists yet.
    pub default_tex: Option<TexFn>,
}

/// Registry of all known block types.
pub struct BlockRegistry {
    defs: Vec<BlockDef>,
}

impl BlockRegistry {
    /// Create a registry pre-populated with the built-in blocks.
    pub fn new() -> Self {
        let mut r = BlockRegistry { defs: Vec::new() };
        // Order matters: defines the AIR/STONE/DIRT/GRASS/LOG/LEAVES ids above.
        r.register("air", false, false, 0.0, None);
        r.register("stone", true, true, 1.2, Some(tex_stone));
        r.register("dirt", true, true, 0.5, Some(tex_dirt));
        r.register("grass", true, true, 0.5, Some(tex_grass));
        r.register("log", true, true, 1.0, Some(tex_log));
        r.register("leaves", true, true, 0.3, Some(tex_leaves));
        r
    }

    /// Register a new block and return its assigned id.
    pub fn register(
        &mut self,
        name: &'static str,
        solid: bool,
        visible: bool,
        break_secs: f32,
        default_tex: Option<TexFn>,
    ) -> BlockId {
        let id = self.defs.len() as BlockId;
        self.defs.push(BlockDef {
            id,
            name,
            solid,
            visible,
            break_secs,
            default_tex,
        });
        id
    }

    pub fn get(&self, id: BlockId) -> &BlockDef {
        // Unknown ids fall back to air so corrupt data can't panic the game.
        self.defs.get(id as usize).unwrap_or(&self.defs[0])
    }

    pub fn is_solid(&self, id: BlockId) -> bool {
        self.get(id).solid
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &BlockDef> {
        self.defs.iter()
    }
}

impl Default for BlockRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Render a procedural [`TexFn`] into a 16x16 RGBA buffer (row-major).
pub fn render_default(tex: TexFn) -> Vec<u8> {
    let mut buf = vec![0u8; (TILE_TEX * TILE_TEX * 4) as usize];
    for y in 0..TILE_TEX {
        for x in 0..TILE_TEX {
            let idx = ((y * TILE_TEX + x) * 4) as usize;
            buf[idx..idx + 4].copy_from_slice(&tex(x, y));
        }
    }
    buf
}

// --- Procedural starter textures -----------------------------------------

fn hash(x: u32, y: u32, salt: u32) -> u32 {
    let mut h = x
        .wrapping_mul(374_761_393)
        .wrapping_add(y.wrapping_mul(668_265_263));
    h = (h ^ (h >> 13))
        .wrapping_mul(1_274_126_177)
        .wrapping_add(salt.wrapping_mul(2_246_822_519));
    h ^ (h >> 16)
}

fn shade(base: [u8; 3], delta: i32) -> [u8; 4] {
    let c = |v: u8| -> u8 { (v as i32 + delta).clamp(0, 255) as u8 };
    [c(base[0]), c(base[1]), c(base[2]), 255]
}

fn tex_stone(x: u32, y: u32) -> [u8; 4] {
    let n = (hash(x, y, 1) % 5) as i32 * 8 - 16;
    shade([120, 120, 128], n)
}

fn tex_dirt(x: u32, y: u32) -> [u8; 4] {
    let n = (hash(x, y, 2) % 5) as i32 * 7 - 14;
    shade([121, 85, 58], n)
}

fn tex_grass(x: u32, y: u32) -> [u8; 4] {
    if y < 4 || (y < 6 && hash(x, y, 4) % 2 == 0) {
        let n = (hash(x, y, 3) % 5) as i32 * 9 - 18;
        shade([83, 150, 60], n)
    } else {
        tex_dirt(x, y)
    }
}

fn tex_log(x: u32, y: u32) -> [u8; 4] {
    // Bark with faint vertical streaks; the two centre columns read as a
    // lighter heartwood core so trunks have a sense of grain.
    let core = if x == 7 || x == 8 { 22 } else { 0 };
    let streak = (hash(x, 0, 7) % 4) as i32 * 6 - 9;
    let n = (hash(x, y, 8) % 3) as i32 * 4 - 4;
    shade([102, 70, 44], core + streak + n)
}

fn tex_leaves(x: u32, y: u32) -> [u8; 4] {
    // Mottled green foliage; occasional darker clumps break up the canopy.
    let dark = if hash(x, y, 10) % 5 == 0 { -22 } else { 0 };
    let n = (hash(x, y, 9) % 6) as i32 * 7 - 17;
    shade([54, 118, 48], n + dark)
}
