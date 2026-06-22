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

/// Built-in item ids. Every id names an *item*; the ones whose [`BlockDef`] is
/// [`placeable`](BlockDef::placeable) are also *blocks* (a block is an item that
/// can additionally be placed in the world). These are guaranteed because
/// [`BlockRegistry::new`] registers them first, in this order.
pub const AIR: BlockId = 0;
pub const STONE: BlockId = 1;
pub const DIRT: BlockId = 2;
pub const GRASS: BlockId = 3;
pub const LOG: BlockId = 4;
pub const LEAVES: BlockId = 5;
/// Planks split from a log. A placeable block.
pub const WOOD: BlockId = 6;
/// Bark stripped from a log. An item (not placeable).
pub const BARK: BlockId = 7;
/// A stick, dropped by leaves and used to craft tools. An item.
pub const STICK: BlockId = 8;
/// A wooden pickaxe. A tool item; stacks to one (see [`max_stack`]).
pub const PICKAXE: BlockId = 9;
/// A stone pickaxe. A tool item; stacks to one.
pub const STONE_PICKAXE: BlockId = 10;
/// Iron ore, generated underground and in mountains. A block; mined with a
/// stone or iron pickaxe, it drops [`RAW_IRON`].
pub const IRON_ORE: BlockId = 11;
/// Raw iron, dropped by [`IRON_ORE`]. An item; smelted at a [`FORGE`] into an
/// [`IRON_INGOT`].
pub const RAW_IRON: BlockId = 12;
/// A refined iron ingot, smelted from [`RAW_IRON`]. An item; used to craft an
/// [`IRON_PICKAXE`].
pub const IRON_INGOT: BlockId = 13;
/// A forge. A placeable block crafted from stone; right-click it to open the
/// smelting GUI.
pub const FORGE: BlockId = 14;
/// An iron pickaxe. A tool item; stacks to one. The fastest pickaxe.
pub const IRON_PICKAXE: BlockId = 15;

/// Generates an RGBA texel at `(x, y)` (both in `0..TILE_TEX`). Used only to
/// seed a starter PNG when the real texture file is missing.
pub type TexFn = fn(x: u32, y: u32) -> [u8; 4];

/// Definition of a single block type.
pub struct BlockDef {
    pub id: BlockId,
    pub name: &'static str,
    /// Whether the player collides with this block.
    pub solid: bool,
    /// Whether the item has a tile in the texture atlas — drawn either as a world
    /// block or as a dropped-item sprite (air is not).
    pub visible: bool,
    /// Whether this item can be placed in the world as a block. Plain items
    /// (bark, sticks, tools) are not placeable; only blocks are.
    pub placeable: bool,
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
        // Order matters: defines the ids declared above.
        //               name             solid  visible placeable break  tex
        r.register("air", false, false, false, 0.0, None);
        r.register("stone", true, true, true, 1.2, Some(tex_stone));
        r.register("dirt", true, true, true, 0.5, Some(tex_dirt));
        r.register("grass", true, true, true, 0.5, Some(tex_grass));
        r.register("log", true, true, true, 1.0, Some(tex_log));
        r.register("leaves", true, true, true, 0.3, Some(tex_leaves));
        // Crafted items. `wood` is a placeable block; the rest are plain items
        // (visible only so their dropped-on-the-ground sprite has an atlas tile).
        r.register("wood", true, true, true, 0.8, Some(tex_wood));
        r.register("bark", false, true, false, 0.0, Some(tex_bark));
        r.register("stick", false, true, false, 0.0, Some(tex_stick));
        r.register("pickaxe", false, true, false, 0.0, Some(tex_pickaxe));
        r.register(
            "stone_pickaxe",
            false,
            true,
            false,
            0.0,
            Some(tex_stone_pickaxe),
        );
        // Iron: ore block, its raw drop, the smelted ingot, the forge that
        // smelts it, and the iron pickaxe it crafts into.
        r.register("iron_ore", true, true, true, 2.0, Some(tex_iron_ore));
        r.register("raw_iron", false, true, false, 0.0, Some(tex_raw_iron));
        r.register("iron_ingot", false, true, false, 0.0, Some(tex_iron_ingot));
        r.register("forge", true, true, true, 1.5, Some(tex_forge));
        r.register(
            "iron_pickaxe",
            false,
            true,
            false,
            0.0,
            Some(tex_iron_pickaxe),
        );
        r
    }

    /// Register a new item and return its assigned id. A `placeable` item is a
    /// block (it can be set into a world cell); the rest are inventory-only.
    pub fn register(
        &mut self,
        name: &'static str,
        solid: bool,
        visible: bool,
        placeable: bool,
        break_secs: f32,
        default_tex: Option<TexFn>,
    ) -> BlockId {
        let id = self.defs.len() as BlockId;
        self.defs.push(BlockDef {
            id,
            name,
            solid,
            visible,
            placeable,
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

    /// Whether `id` is a block that can be placed into the world.
    pub fn is_placeable(&self, id: BlockId) -> bool {
        self.get(id).placeable
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

/// Maximum number of `id` that fit in one inventory slot. Tools stack to one;
/// everything else uses the default [`STACK_MAX`](crate::inventory::STACK_MAX).
pub fn max_stack(id: BlockId) -> u32 {
    match id {
        PICKAXE | STONE_PICKAXE | IRON_PICKAXE => 1,
        _ => crate::inventory::STACK_MAX,
    }
}

// --- Tools & mining ------------------------------------------------------

/// Mining tier of a held item: the strength of a pickaxe, or `0` for bare hands
/// and anything that isn't a pickaxe. Higher tiers mine faster and can harvest
/// tougher blocks (see [`required_tier`]). Wood `1` < stone `2` < iron `3`.
pub fn pickaxe_tier(item: BlockId) -> u8 {
    match item {
        PICKAXE => 1,
        STONE_PICKAXE => 2,
        IRON_PICKAXE => 3,
        _ => 0,
    }
}

/// Minimum [`pickaxe_tier`] needed to mine `block` quickly and have it drop.
/// `0` means no pickaxe is required. A block can still be chipped at by hand or
/// with too weak a tool, but only very slowly and without yielding a drop.
pub fn required_tier(block: BlockId) -> u8 {
    match block {
        STONE => 1,    // any pickaxe
        IRON_ORE => 2, // stone or iron pickaxe only
        _ => 0,
    }
}

/// Multiplier applied to a block's [`break_secs`](BlockDef::break_secs) given
/// the item the player is holding (`held`; [`AIR`] means bare hands). Smaller is
/// faster. Mining a tool-gated block with too weak a tool is punishingly slow;
/// the right tool (or better) speeds it up, more so at higher tiers.
pub fn mine_speed_mult(block: BlockId, held: BlockId) -> f32 {
    let req = required_tier(block);
    if req == 0 {
        return 1.0;
    }
    let tier = pickaxe_tier(held);
    if tier < req {
        return 5.0; // wrong/no tool: very long
    }
    match tier {
        1 => 0.6,
        2 => 0.3,
        _ => 0.18, // iron (tier 3) and up
    }
}

/// Whether breaking `block` while holding `held` yields a dropped item. A
/// tool-gated block only drops when mined with a pickaxe of at least its
/// [`required_tier`].
pub fn drops_when_mined(block: BlockId, held: BlockId) -> bool {
    pickaxe_tier(held) >= required_tier(block)
}

/// The item a broken `block` drops (assuming [`drops_when_mined`]). Most blocks
/// drop themselves; a few transform (leaves shed a stick, iron ore yields raw
/// iron).
pub fn mined_drop(block: BlockId) -> BlockId {
    match block {
        LEAVES => STICK,
        IRON_ORE => RAW_IRON,
        other => other,
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

fn tex_wood(x: u32, y: u32) -> [u8; 4] {
    // Planks: light sawn timber with horizontal seams every few rows.
    let seam = if y % 5 == 0 { -30 } else { 0 };
    let n = (hash(x, y, 11) % 4) as i32 * 5 - 7;
    shade([176, 138, 88], seam + n)
}

fn tex_bark(x: u32, y: u32) -> [u8; 4] {
    // A curled strip of bark: dark, with rough vertical ridges.
    let ridge = if x % 3 == 0 { -16 } else { 0 };
    let n = (hash(x, y, 12) % 4) as i32 * 6 - 9;
    shade([84, 56, 34], ridge + n)
}

fn tex_stick(x: u32, y: u32) -> [u8; 4] {
    // A single diagonal twig over transparency.
    if x.abs_diff(y) <= 1 {
        let n = (hash(x, y, 13) % 3) as i32 * 6 - 6;
        shade([138, 96, 54], n)
    } else {
        [0, 0, 0, 0]
    }
}

fn tex_pickaxe(x: u32, y: u32) -> [u8; 4] {
    pickaxe_tex(x, y, [150, 110, 70], [200, 200, 210])
}

fn tex_stone_pickaxe(x: u32, y: u32) -> [u8; 4] {
    pickaxe_tex(x, y, [138, 96, 54], [120, 120, 128])
}

fn tex_iron_pickaxe(x: u32, y: u32) -> [u8; 4] {
    pickaxe_tex(x, y, [138, 96, 54], [214, 210, 205])
}

fn tex_iron_ore(x: u32, y: u32) -> [u8; 4] {
    // Stone speckled with tan iron nuggets.
    if hash(x, y, 20) % 6 == 0 {
        let n = (hash(x, y, 21) % 4) as i32 * 8 - 8;
        shade([196, 152, 104], n)
    } else {
        tex_stone(x, y)
    }
}

fn tex_raw_iron(x: u32, y: u32) -> [u8; 4] {
    // A rough rusty-tan nugget over transparency.
    let cx = x as i32 - 8;
    let cy = y as i32 - 8;
    if cx * cx + cy * cy <= 30 {
        let n = (hash(x, y, 22) % 5) as i32 * 7 - 14;
        shade([170, 130, 96], n)
    } else {
        [0, 0, 0, 0]
    }
}

fn tex_iron_ingot(x: u32, y: u32) -> [u8; 4] {
    // A metallic bar: a rounded grey block with a lighter top bevel.
    if (3..=12).contains(&x) && (5..=11).contains(&y) {
        let bevel = if y <= 6 { 24 } else { 0 };
        let n = (hash(x, y, 23) % 3) as i32 * 5 - 5;
        shade([196, 198, 205], bevel + n)
    } else {
        [0, 0, 0, 0]
    }
}

fn tex_forge(x: u32, y: u32) -> [u8; 4] {
    // Dark stone block with a glowing mouth in the lower middle.
    let in_mouth = (4..=11).contains(&x) && (8..=13).contains(&y);
    if in_mouth {
        let n = (hash(x, y, 24) % 4) as i32 * 10;
        shade([180, 80, 30], n) // embers
    } else {
        let n = (hash(x, y, 25) % 5) as i32 * 6 - 12;
        shade([84, 82, 88], n) // dark masonry
    }
}

/// Shared pickaxe silhouette: a diagonal handle of `handle` colour with a curved
/// `head` across the top. Transparent elsewhere.
fn pickaxe_tex(x: u32, y: u32, handle: [u8; 3], head: [u8; 3]) -> [u8; 4] {
    // Handle runs from lower-left to upper-right.
    let on_handle = (x + y).abs_diff(16) <= 1 && y >= 3;
    // Head: a bowed bar near the top spanning most of the width.
    let on_head =
        y >= 2 && y <= 4 && (3..=12).contains(&x) && (x as i32 - 7).pow(2) / 6 <= (4 - y) as i32;
    if on_head {
        let n = (hash(x, y, 14) % 3) as i32 * 6 - 6;
        shade(head, n)
    } else if on_handle {
        let n = (hash(x, y, 15) % 3) as i32 * 5 - 5;
        shade(handle, n)
    } else {
        [0, 0, 0, 0]
    }
}
