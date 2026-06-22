//! Block definitions and an extensible registry.
//!
//! Blocks are data-driven: a [`BlockDef`] describes a block's name, whether it
//! is solid, and where to find its 16x16 sprite. New blocks are added by
//! calling [`BlockRegistry::register`] — nothing else in the engine needs to
//! change. A block's id is its index in the registry (and its cell in the
//! texture atlas), so registration order defines ids.
//!
//! Sprites are baked into the binary (see [`crate::assets`]) and decoded by
//! [`crate::client`]'s atlas loader. Each visible block is drawn from the
//! embedded `<name>.png`.

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
/// A wooden sword. A tool item; stacks to one. A dedicated weapon that hits
/// harder than a bare fist (see [`attack_damage`]).
pub const WOOD_SWORD: BlockId = 16;
/// A stone sword. A tool item; stacks to one. Hits harder than the wooden sword.
pub const STONE_SWORD: BlockId = 17;
/// An iron sword. A tool item; stacks to one. The deadliest melee weapon.
pub const IRON_SWORD: BlockId = 18;

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
        //               name             solid  visible placeable break
        r.register("air", false, false, false, 0.0);
        r.register("stone", true, true, true, 1.2);
        r.register("dirt", true, true, true, 0.5);
        r.register("grass", true, true, true, 0.5);
        r.register("log", true, true, true, 1.0);
        r.register("leaves", true, true, true, 0.3);
        // Crafted items. `wood` is a placeable block; the rest are plain items
        // (visible only so their dropped-on-the-ground sprite has an atlas tile).
        r.register("wood", true, true, true, 0.8);
        r.register("bark", false, true, false, 0.0);
        r.register("stick", false, true, false, 0.0);
        r.register("pickaxe", false, true, false, 0.0);
        r.register("stone_pickaxe", false, true, false, 0.0);
        // Iron: ore block, its raw drop, the smelted ingot, the forge that
        // smelts it, and the iron pickaxe it crafts into.
        r.register("iron_ore", true, true, true, 2.0);
        r.register("raw_iron", false, true, false, 0.0);
        r.register("iron_ingot", false, true, false, 0.0);
        r.register("forge", true, true, true, 1.5);
        r.register("iron_pickaxe", false, true, false, 0.0);
        // Swords: dedicated melee weapons, wood < stone < iron.
        r.register("wood_sword", false, true, false, 0.0);
        r.register("stone_sword", false, true, false, 0.0);
        r.register("iron_sword", false, true, false, 0.0);
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
    ) -> BlockId {
        let id = self.defs.len() as BlockId;
        self.defs.push(BlockDef {
            id,
            name,
            solid,
            visible,
            placeable,
            break_secs,
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
        PICKAXE | STONE_PICKAXE | IRON_PICKAXE | WOOD_SWORD | STONE_SWORD | IRON_SWORD => 1,
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

// --- Durability ----------------------------------------------------------

/// Maximum durability (uses before it breaks) of a tool, or `0` for anything
/// without durability (blocks, materials, bare hands). Higher tiers last
/// longer. A fresh tool starts at this value; each use spends some (see
/// [`attack_wear`] / [`mine_wear`]) and it shatters at zero.
pub fn max_durability(item: BlockId) -> u16 {
    match item {
        PICKAXE | WOOD_SWORD => 60,
        STONE_PICKAXE | STONE_SWORD => 132,
        IRON_PICKAXE | IRON_SWORD => 251,
        _ => 0,
    }
}

/// Whether `item` is a pickaxe (mining is its intended use).
pub fn is_pickaxe(item: BlockId) -> bool {
    matches!(item, PICKAXE | STONE_PICKAXE | IRON_PICKAXE)
}

/// Whether `item` is a sword (attacking is its intended use).
pub fn is_sword(item: BlockId) -> bool {
    matches!(item, WOOD_SWORD | STONE_SWORD | IRON_SWORD)
}

/// Durability spent attacking with `item`. A sword's intended use costs `1`; a
/// pickaxe swung as a weapon wears twice as fast. Anything else costs nothing.
pub fn attack_wear(item: BlockId) -> u16 {
    if is_sword(item) {
        1
    } else if is_pickaxe(item) {
        2
    } else {
        0
    }
}

/// Durability spent mining with `item`. A pickaxe's intended use costs `1`; a
/// sword used to dig wears twice as fast. Anything else costs nothing.
pub fn mine_wear(item: BlockId) -> u16 {
    if is_pickaxe(item) {
        1
    } else if is_sword(item) {
        2
    } else {
        0
    }
}

/// The material that repairs `item` at a [`FORGE`] — the same material it is
/// crafted from — or `None` if `item` has no durability to repair.
pub fn repair_material(item: BlockId) -> Option<BlockId> {
    match item {
        PICKAXE | WOOD_SWORD => Some(WOOD),
        STONE_PICKAXE | STONE_SWORD => Some(STONE),
        IRON_PICKAXE | IRON_SWORD => Some(IRON_INGOT),
        _ => None,
    }
}

/// Durability restored per unit of material spent repairing `item`. A quarter
/// of its maximum (rounded up), so a fully-worn tool takes four materials to
/// mend back to new.
pub fn repair_step(item: BlockId) -> u16 {
    max_durability(item).div_ceil(4)
}

/// Melee damage dealt by a swing while holding `item` ([`AIR`] for bare hands).
/// Swords are dedicated weapons and hit hardest; pickaxes can still be swung but
/// deal far less, and anything else (including bare hands) deals the base amount.
/// Wood < stone < iron within each family.
pub fn attack_damage(item: BlockId) -> i32 {
    match item {
        WOOD_SWORD => 8,
        STONE_SWORD => 11,
        IRON_SWORD => 14,
        PICKAXE => 4,
        STONE_PICKAXE => 5,
        IRON_PICKAXE => 6,
        _ => 3, // bare hands or any non-weapon item
    }
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
