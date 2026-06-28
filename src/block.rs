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
/// A ladder. A placeable, non-solid block mounted on the side of a wall and
/// climbed vertically with the jump/down inputs (see [`is_climbable`]).
pub const LADDER: BlockId = 19;
/// An apple. A food item occasionally shed by [`LEAVES`]; eaten to restore
/// health (see [`food_heal`]).
pub const APPLE: BlockId = 20;
/// Raw animal meat, dropped by slain chickens and goats. A food item, but eating
/// it raw makes you sick (it *costs* health); cook it on a [`CAMPFIRE`] first.
pub const RAW_MEAT: BlockId = 21;
/// Cooked meat, made by cooking [`RAW_MEAT`] on a lit [`CAMPFIRE`]. A food item
/// that restores a hearty amount of health.
pub const COOKED_MEAT: BlockId = 22;
/// A campfire (unlit). A placeable block crafted from stone and bark; feed it
/// wood or bark to light it ([`CAMPFIRE_LIT`]) and cook raw meat on it.
pub const CAMPFIRE: BlockId = 23;
/// A lit campfire. The burning state of a [`CAMPFIRE`]: never held as an item
/// (you light a placed campfire by adding fuel), it reverts to [`CAMPFIRE`] when
/// its fuel runs out. Raw meat can only be cooked while a campfire is lit.
pub const CAMPFIRE_LIT: BlockId = 24;
/// A wooden axe. A tool item; stacks to one. Hits harder than a sword but wears
/// twice as fast, and fells whole trees (see [`is_axe`]).
pub const WOOD_AXE: BlockId = 25;
/// A stone axe. A tool item; stacks to one. Hits harder than the wooden axe.
pub const STONE_AXE: BlockId = 26;
/// An iron axe. A tool item; stacks to one. The deadliest axe.
pub const IRON_AXE: BlockId = 27;
/// Rope, twisted from [`BARK`]. A plain item used to craft a [`ROPE_LADDER`].
pub const ROPE: BlockId = 28;
/// A rope ladder. A placeable, non-solid, climbable block meant for cave diving:
/// placing one unrolls a run of rope downward until it hits the cave floor or
/// reaches its length limit (see [`crate::server`]). Drop another onto the bottom
/// of a spent run to continue the descent. Crafted from [`ROPE`].
pub const ROPE_LADDER: BlockId = 29;
/// Coal ore, generated underground like [`IRON_ORE`] but more abundant and minable
/// with any pickaxe. A block; mined, it drops [`COAL`].
pub const COAL_ORE: BlockId = 30;
/// A lump of coal, dropped by [`COAL_ORE`]. An item used as fuel — it burns in a
/// [`CAMPFIRE`] and powers a [`FORGE`] (see [`fuel_seconds`] / [`forge_fuel_units`]).
pub const COAL: BlockId = 31;
/// Water. A non-solid, non-placeable fluid that floods low-lying basins and
/// valleys up to sea level (see [`crate::worldgen`]). The player swims through it
/// rather than walking on it: it slows movement, lets them paddle up and down,
/// and cushions any fall (see [`crate::client`]'s physics). Not minable by hand.
pub const WATER: BlockId = 32;
/// An empty bucket, forged from iron ingots. An item (not placeable): right-click
/// a [`WATER`] cell with it to scoop the water up, turning it into a
/// [`WATER_BUCKET`] (see [`crate::server`]'s bucket handling).
pub const BUCKET: BlockId = 33;
/// A bucket full of water. An item: right-click an empty cell with it to pour the
/// water out, placing a [`WATER`] block and leaving the empty [`BUCKET`] behind.
/// Stacks to one, since each bucket carries a single load.
pub const WATER_BUCKET: BlockId = 34;
/// Charred rock: the terrain block of the underworld, the fire-scorched stone the
/// whole layer beneath the overworld is built from. A solid, placeable block mined
/// with any pickaxe like ordinary stone.
pub const CHARRED_ROCK: BlockId = 35;
/// Fire: a non-solid, non-placeable hazard block. It dots the underworld and is
/// laid down in a trail by a [`crate::entity::EntityKind::CharredSkeleton`] chasing
/// its prey. Standing in it burns the player (see [`is_fire`] and the server's fire
/// tick). Walked through, not mined: it carries no breaking delay and drops nothing.
pub const FIRE: BlockId = 36;
/// Tungsten ore, generated only in the underworld's charred rock. A block; its
/// stone is tough, so it yields [`RAW_TUNGSTEN`] only to an [`IRON_PICKAXE`] or
/// better (see [`required_tier`]).
pub const TUNGSTEN_ORE: BlockId = 37;
/// Raw tungsten, dropped by [`TUNGSTEN_ORE`]. An item; smelted at a [`FORGE`] into
/// a [`TUNGSTEN_INGOT`].
pub const RAW_TUNGSTEN: BlockId = 38;
/// A refined tungsten ingot, smelted from [`RAW_TUNGSTEN`]. An item; used to craft
/// the strongest tools and weapons in the game.
pub const TUNGSTEN_INGOT: BlockId = 39;
/// A tungsten pickaxe. A tool item; stacks to one. The fastest, longest-lasting
/// pickaxe — stronger than iron (see [`pickaxe_tier`] / [`max_durability`]).
pub const TUNGSTEN_PICKAXE: BlockId = 40;
/// A tungsten sword. A tool item; stacks to one. The deadliest melee weapon,
/// out-hitting iron.
pub const TUNGSTEN_SWORD: BlockId = 41;
/// A tungsten axe. A tool item; stacks to one. The deadliest axe, out-hitting iron.
pub const TUNGSTEN_AXE: BlockId = 42;
/// A door (closed, lower half): both the crafted item the player holds and the
/// placed lower cell of a two-tall door. A solid, placeable block — placing one
/// raises a [`DOOR_TOP`] in the cell above, so the door stands two cells tall and
/// blocks movement like a wall. Right-click it to swing it open ([`DOOR_OPEN`]).
pub const DOOR: BlockId = 43;
/// The upper half of a closed door. World-only (never held): set in the cell
/// directly above a placed [`DOOR`]. Solid, like its lower half.
pub const DOOR_TOP: BlockId = 44;
/// The lower half of an open door. World-only and non-solid — the player steps
/// through an open doorway. Right-click it to swing the door shut ([`DOOR`]).
pub const DOOR_OPEN: BlockId = 45;
/// The upper half of an open door. World-only and non-solid, paired with a
/// [`DOOR_OPEN`] below.
pub const DOOR_OPEN_TOP: BlockId = 46;
/// Sand: the loose surface block of the overworld's desert biome. A solid,
/// placeable block dug quickly by hand, like dirt.
pub const SAND: BlockId = 47;
/// Ash: the powdery surface block of the underworld's ash valleys. A solid,
/// placeable block dug quickly by hand, like dirt.
pub const ASH: BlockId = 48;
/// The fire key: a dimension-crossing artifact crafted from a [`CHARRED_ROCK`]
/// and a [`TUNGSTEN_INGOT`]. An item (not placeable); right-click while holding
/// it to warp between the overworld and the underworld (see [`crate::server`]'s
/// fire-key handling). Reusable — using it does not consume the key.
pub const FIRE_KEY: BlockId = 49;
/// Stone bricks: a decorative building block crafted from [`STONE`]. Mechanically
/// identical to stone — a solid, placeable block mined with any pickaxe and as
/// tough to break — it just wears a tidier, dressed-stone face.
pub const STONE_BRICKS: BlockId = 50;
/// A boat, crafted from wood and sticks. An item (not placeable, stacks to one):
/// right-click while holding it to climb aboard and glide across [`WATER`], and
/// right-click again to step back out. Riding is a client-side movement mode (see
/// [`crate::client`]); the boat is a vehicle, never consumed by using it.
pub const BOAT: BlockId = 51;
/// A sign: a non-solid, placeable wooden board the player writes on. Right-click a
/// placed sign to open its editor (see [`crate::client`]) and inscribe up to
/// [`crate::protocol::TEXT_ROWS`] lines of text, stored per cell by the server.
pub const SIGN: BlockId = 52;
/// A quest board: like a [`SIGN`] but holding several notes rather than one. A
/// non-solid, placeable block; right-click it to read and post up to
/// [`crate::protocol::QUEST_MAX_NOTES`] notes, each its own short message.
pub const QUEST_BOARD: BlockId = 53;
/// Gold ore: a glittering vein found only in the underworld's charred rock. A
/// solid, placeable block that yields [`RAW_GOLD`] when mined with a stone pickaxe
/// or better. Gold is too soft for tools or weapons — its only use is reinforcing
/// chests into locked chests (see [`LOCKED_CHEST`]).
pub const GOLD_ORE: BlockId = 54;
/// Raw gold, dropped by [`GOLD_ORE`]. An item; smelted at a [`FORGE`] into a
/// [`GOLD_INGOT`].
pub const RAW_GOLD: BlockId = 55;
/// A refined gold ingot, smelted from [`RAW_GOLD`]. An item used only to reinforce
/// a placed [`CHEST`] into a [`LOCKED_CHEST`]; it crafts into no tools or weapons.
pub const GOLD_INGOT: BlockId = 56;
/// A chest: a solid, placeable storage block holding [`crate::inventory::CHEST_SLOTS`]
/// slots. Right-click to open it. Reinforce a placed chest with [`GOLD_INGOT`]s to
/// turn it into a [`LOCKED_CHEST`].
pub const CHEST: BlockId = 57;
/// A locked chest: a [`CHEST`] reinforced with gold and sealed with a password.
/// It cannot be opened or broken without the password. Never held as an item —
/// it only exists in the world, created by reinforcing a placed chest.
pub const LOCKED_CHEST: BlockId = 58;
/// The arena key: a dimension-crossing artifact crafted from a [`TUNGSTEN_INGOT`]
/// and a [`GOLD_INGOT`]. An item (not placeable); right-click while holding it to
/// warp into the [`crate::world::Dimension::Arena`] — the stone-brick boss arena —
/// and right-click again to return to where you left from (see [`crate::server`]'s
/// arena-key handling). Reusable — using it does not consume the key.
pub const ARENA_KEY: BlockId = 59;
/// A full suit of iron armor, forged from a hoard of [`IRON_INGOT`]s. An item (not
/// placeable, stacks to one): a single piece worn as a whole suit — there are no
/// individual greaves or helms. Carrying one shields you, blunting incoming attack
/// damage (see [`armor_defense`]); it raises your defense, never your health.
pub const IRON_ARMOR: BlockId = 60;
/// A full suit of tungsten armor, forged from a hoard of [`TUNGSTEN_INGOT`]s — the
/// sturdiest armor in the world, turning aside more of a blow than iron. An item
/// (not placeable, stacks to one) worn as a whole suit. Also spilled by a slain
/// [`crate::entity::EntityKind::DemonKing`] (in its chest) and, rarely, a
/// [`crate::entity::EntityKind::DarkKnight`].
pub const TUNGSTEN_ARMOR: BlockId = 61;
/// A dragon scale: a shimmering plate shed by a slain
/// [`crate::entity::EntityKind::Dragon`], the underworld's rare flying miniboss.
/// An item (not placeable); five of them, alongside a tungsten and a gold ingot,
/// are crafted into an [`ARENA_KEY`].
pub const DRAGON_SCALE: BlockId = 62;
/// The summoner spellbook: the first of the world's spellbooks and the seed of its
/// magic system. An item (not placeable, stacks to one) rarely spilled by a slain
/// [`crate::entity::EntityKind::Necromancer`]. Right-click while holding it to spend
/// [`SUMMONER_SPELL_MANA_COST`] mana and loose a friendly summoner fireball that
/// bursts into a bouncing skull which hunts *monsters* on your behalf (see
/// [`crate::entity::EntityKind::FriendlySkull`]). Reusable — casting never consumes
/// the book, only mana (won by slaying monsters; see [`crate::server`]).
pub const SUMMONER_SPELL: BlockId = 63;
/// The sunburst spellbook: a spellbook found in the loot chest of a ruin (see
/// [`crate::worldgen`]). An item (not placeable, stacks to one). Right-click while
/// holding it to spend [`SUNBURST_SPELL_MANA_COST`] mana and loose a burst of
/// sunlight that **instantly slays every creature that burns in daylight** (zombies,
/// skeletons, dark knights, necromancers and their skulls) within ten blocks.
/// Reusable — casting never consumes the book, only mana.
pub const SUNBURST_SPELL: BlockId = 64;
/// The restore spellbook: a spellbook found in the [demon king](crate::entity::EntityKind::DemonKing)'s
/// loot chest. An item (not placeable, stacks to one). Right-click while holding it
/// to spend [`RESTORE_SPELL_MANA_COST`] mana and **restore** the creature under your
/// cursor: an orc or a dark knight becomes a [`knight`](crate::entity::EntityKind::Knight),
/// a dark musketeer becomes a [`musketeer`](crate::entity::EntityKind::Musketeer),
/// an orc mage becomes a [`mage`](crate::entity::EntityKind::Mage), and an enchanted
/// demon is calmed back into an ordinary one. A knight, musketeer or mage you restore is
/// **recruited** to you. Reusable — casting never consumes the book, only mana.
pub const RESTORE_SPELL: BlockId = 65;
/// The dragonian steed spellbook: a spellbook found in the
/// [demon king](crate::entity::EntityKind::DemonKing)'s loot chest, alongside the
/// [`RESTORE_SPELL`] — too potent to leave to the rank-and-file
/// [`dragon`](crate::entity::EntityKind::Dragon) miniboss it raises a tamed twin of, it
/// rests with the king who once kept dragons as steeds. An
/// item (not placeable, stacks to one). Right-click while holding it to spend
/// [`DRAGONIAN_STEED_MANA_COST`] mana and
/// summon a friendly white [`dragon`](crate::entity::EntityKind::WhiteDragon) at your
/// side: it never despawns, soars after you breathing fire at nearby monsters, and can
/// be **ridden** (right-click it) to fly — loosing fireballs at the cursor on the
/// breath key while mounted. Casting again resummons a fresh steed beside you (the old
/// one, if any, vanishes), which is the only way a fallen steed returns. Reusable —
/// casting never consumes the book, only mana.
pub const DRAGONIAN_STEED: BlockId = 66;
/// A sheet of paper, pressed from [`BARK`]. An item (not placeable). Its sole use
/// is **cloning spellbooks**: a spellbook bound together with a sheaf of paper
/// transcribes itself into a second identical tome (see [`crate::recipe::RECIPES`]).
pub const PAPER: BlockId = 67;
/// A musket: a ranged firearm crafted from [`WOOD`] and an [`IRON_INGOT`]. A tool
/// item (not placeable, stacks to one). Right-click while holding it to fire a
/// [`crate::entity::EntityKind::FriendlyBullet`] toward the cursor, spending one
/// [`BULLET`] from the inventory as ammunition (see [`crate::server`]'s musket
/// handling). Reusable — firing consumes a bullet, never the musket itself.
pub const MUSKET: BlockId = 68;
/// A bullet: ammunition for the [`MUSKET`], crafted in batches from an
/// [`IRON_INGOT`]. An item (not placeable). Firing a musket spends one bullet; a
/// player out of bullets can't shoot.
pub const BULLET: BlockId = 69;
/// A minotaur horn: a great curved horn wrenched from a slain
/// [`crate::entity::EntityKind::Minotaur`], the underworld's rare horned miniboss. An
/// item (not placeable); five of them, alongside a clutch of [`DRAGON_SCALE`]s and a
/// tungsten and gold ingot, are crafted into an [`ARENA_KEY`].
pub const MINOTAUR_HORN: BlockId = 70;
/// The dragon plate spellbook: the rarest of the world's spellbooks, dropped only by
/// the post-game superboss [`crate::entity::EntityKind::Twinscale`] (and cloneable
/// thereafter). An item (not placeable, stacks to one). Right-click while holding it
/// to spend [`DRAGON_PLATE_SPELL_MANA_COST`] mana and wreath yourself in a scaled
/// ward: for [`DRAGON_PLATE_BUFF_DURATION`] seconds your *defense* is raised to
/// [`DRAGON_PLATE_DEFENSE`] — far higher than any forged suit (more even than
/// tungsten) — turning aside the great bulk of every blow. The ward is pure magic,
/// not metal: it ignores (and never wears down) any armor you carry, and it does not
/// stack with it. When the eight minutes lapse the ward fades and you fall back on
/// ordinary armor; recast to renew it. Reusable — casting never consumes the book,
/// only mana.
pub const DRAGON_PLATE_SPELL: BlockId = 71;

/// Gold ingots consumed to reinforce a plain [`CHEST`] into a [`LOCKED_CHEST`].
/// Shared by the server (which charges it) and the client (which shows the cost).
pub const LOCKED_CHEST_GOLD_COST: u32 = 3;

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
        // A ladder: a placeable but non-solid block you climb through.
        r.register("ladder", false, true, true, 0.4);
        // Food: an apple (shed by leaves) and raw/cooked meat (dropped by
        // animals, cooked on a campfire). All non-solid inventory items.
        r.register("apple", false, true, false, 0.0);
        r.register("raw_meat", false, true, false, 0.0);
        r.register("cooked_meat", false, true, false, 0.0);
        // A campfire and its lit state. Non-solid (you stand in it) placeable
        // blocks; `campfire_lit` is never held, only the world-side burning form.
        r.register("campfire", false, true, true, 0.6);
        r.register("campfire_lit", false, true, false, 0.6);
        // Axes: dedicated tree-felling weapons, wood < stone < iron. Hit harder
        // than swords but wear twice as fast.
        r.register("wood_axe", false, true, false, 0.0);
        r.register("stone_axe", false, true, false, 0.0);
        r.register("iron_axe", false, true, false, 0.0);
        // Rope (an item) and the rope ladder it crafts into: a non-solid,
        // climbable block that unrolls down a cave shaft when placed.
        r.register("rope", false, true, false, 0.0);
        r.register("rope_ladder", false, true, true, 0.4);
        // Coal: an ore block (minable with any pickaxe) and the fuel lump it drops.
        r.register("coal_ore", true, true, true, 1.5);
        r.register("coal", false, true, false, 0.0);
        // Water: a non-solid fluid pooling in low terrain at sea level. Swum
        // through, not walked on; neither minable nor placeable.
        r.register("water", false, true, false, 0.0);
        // Buckets: an iron-forged carrier and its water-filled form. Plain items
        // (not placed as blocks); right-clicking a cell scoops or pours water.
        r.register("bucket", false, true, false, 0.0);
        r.register("water_bucket", false, true, false, 0.0);
        // Charred rock: the underworld's terrain, a solid placeable block dug like
        // stone. Fire: a non-solid, non-placeable hazard (walked through, not built
        // with), so it is registered unplaceable with no breaking delay.
        r.register("charred_rock", true, true, true, 1.3);
        r.register("fire", false, true, false, 0.0);
        // Tungsten: the underworld's ore (tougher than iron ore to break), its raw
        // drop, the smelted ingot, and the strongest tools forged from it.
        r.register("tungsten_ore", true, true, true, 2.6);
        r.register("raw_tungsten", false, true, false, 0.0);
        r.register("tungsten_ingot", false, true, false, 0.0);
        r.register("tungsten_pickaxe", false, true, false, 0.0);
        r.register("tungsten_sword", false, true, false, 0.0);
        r.register("tungsten_axe", false, true, false, 0.0);
        // A door: a two-tall barrier that swings open and shut. The lower half
        // (`door`) is the crafted, held, placeable block; placing it raises a
        // `door_top` above. The closed halves are solid; the open halves
        // (`door_open`/`door_open_top`) are non-solid so the player steps through.
        // Only the closed lower half is ever held, so the rest are world-only.
        r.register("door", true, true, true, 0.6);
        r.register("door_top", true, true, false, 0.6);
        r.register("door_open", false, true, false, 0.6);
        r.register("door_open_top", false, true, false, 0.6);
        // Biome surface blocks: sand for the overworld desert, ash for the
        // underworld's ash valleys. Both loose ground, dug quickly like dirt.
        r.register("sand", true, true, true, 0.5);
        r.register("ash", true, true, true, 0.5);
        // The fire key: a non-placeable artifact item (visible only so its
        // dropped sprite has an atlas tile). Right-clicking with it warps the
        // player between dimensions.
        r.register("fire_key", false, true, false, 0.0);
        // Stone bricks: a crafted decorative block that behaves just like stone.
        r.register("stone_bricks", true, true, true, 1.2);
        // A boat: a non-placeable vehicle item (visible only so its inventory and
        // dropped sprite have an atlas tile). Ridden by right-clicking, not placed.
        r.register("boat", false, true, false, 0.0);
        // A sign and a quest board: non-solid placeable boards the player writes
        // on. Right-clicking a placed one opens its text editor (the text itself is
        // stored per cell by the server, not in the block id).
        r.register("sign", false, true, true, 0.5);
        r.register("quest_board", false, true, true, 0.6);
        // Gold: an underworld ore, its raw drop, and the smelted ingot. Gold makes
        // no tools or weapons — its sole use is reinforcing chests (see below).
        r.register("gold_ore", true, true, true, 2.2);
        r.register("raw_gold", false, true, false, 0.0);
        r.register("gold_ingot", false, true, false, 0.0);
        // Chests: a normal storage chest, and the locked chest a placed chest
        // becomes once reinforced with gold. The locked form is never held as an
        // item (it is created in place), so it is registered unplaceable.
        r.register("chest", true, true, true, 1.0);
        r.register("locked_chest", true, true, false, 1.2);
        // The arena key: a non-placeable artifact item (visible only so its
        // inventory and dropped sprite have an atlas tile). Right-clicking with it
        // warps the player into and out of the stone-brick boss arena.
        r.register("arena_key", false, true, false, 0.0);
        // Armor suits: whole-body iron and tungsten armor, worn as a single piece
        // (no separate helm/greaves). Non-placeable items that stack to one; worn
        // simply by carrying one, they blunt incoming attack damage.
        r.register("iron_armor", false, true, false, 0.0);
        r.register("tungsten_armor", false, true, false, 0.0);
        // A dragon scale: a non-placeable item dropped by a slain dragon, crafted
        // (in fives) into an arena key.
        r.register("dragon_scale", false, true, false, 0.0);
        // The summoner spellbook: a non-placeable artifact item (visible only so its
        // inventory and dropped sprite have an atlas tile). Right-clicking with it
        // spends mana to cast the summoner spell, rather than placing a block.
        r.register("summoner_spell", false, true, false, 0.0);
        // The sunburst spellbook: another non-placeable artifact item, found in a
        // ruin's loot chest. Cast (right-click) to burst daylight-burning undead.
        r.register("sunburst_spell", false, true, false, 0.0);
        // The restore spellbook: a non-placeable artifact item from the demon king's
        // chest. Cast (right-click) to restore the creature under the cursor.
        r.register("restore_spell", false, true, false, 0.0);
        // The dragonian steed spellbook: a non-placeable artifact item, spilled by a
        // slain dragon. Right-clicking it spends mana to summon a friendly white
        // dragon, rather than placing a block.
        r.register("dragonian_steed_spell", false, true, false, 0.0);
        // Paper: a non-placeable item pressed from bark, used to clone spellbooks.
        r.register("paper", false, true, false, 0.0);
        // A musket: a non-placeable ranged weapon (a tool, stacks to one). Right-click
        // fires a bullet toward the cursor. Bullets: its non-placeable ammunition.
        r.register("musket", false, true, false, 0.0);
        r.register("bullet", false, true, false, 0.0);
        // A minotaur horn: a non-placeable item wrenched from a slain minotaur, crafted
        // (in fives, with dragon scales, gold and tungsten) into an arena key.
        r.register("minotaur_horn", false, true, false, 0.0);
        // The dragon plate spellbook: a non-placeable artifact item, dropped only by
        // Twinscale. Right-clicking it spends mana to ward the caster in a temporary
        // scaled defense far stronger than tungsten, rather than placing a block.
        r.register("dragon_plate_spell", false, true, false, 0.0);
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
        PICKAXE | STONE_PICKAXE | IRON_PICKAXE | TUNGSTEN_PICKAXE | WOOD_SWORD | STONE_SWORD
        | IRON_SWORD | TUNGSTEN_SWORD | WOOD_AXE | STONE_AXE | IRON_AXE | TUNGSTEN_AXE => 1,
        // A water bucket carries a single load, so it never stacks.
        WATER_BUCKET => 1,
        // A boat is a single bulky vehicle, so it never stacks.
        BOAT => 1,
        // A musket is a single bulky firearm, so it never stacks (its bullets do).
        MUSKET => 1,
        // A suit of armor is a single bulky piece, so it never stacks.
        IRON_ARMOR | TUNGSTEN_ARMOR => 1,
        // A spellbook is a single bound tome, so it never stacks.
        SUMMONER_SPELL | SUNBURST_SPELL | RESTORE_SPELL | DRAGONIAN_STEED | DRAGON_PLATE_SPELL => 1,
        _ => crate::inventory::STACK_MAX,
    }
}

/// The percentage of incoming attack damage a suit of armor turns aside while
/// worn — its *defense*. `0` for anything that isn't armor. Armor is worn simply
/// by carrying it (see [`crate::server`]); the sturdier the metal, the more of a
/// blow it blunts. Iron stops better than a third of a hit; tungsten, more than
/// half. Defense never touches your maximum health — it only softens the blows
/// you take. A blow always lands for at least 1 (armor cannot make you invincible).
pub fn armor_defense(item: BlockId) -> i32 {
    match item {
        IRON_ARMOR => 35,
        TUNGSTEN_ARMOR => 55,
        _ => 0,
    }
}

// --- Tools & mining ------------------------------------------------------

/// Mining tier of a held item: the strength of a pickaxe, or `0` for bare hands
/// and anything that isn't a pickaxe. Higher tiers mine faster and can harvest
/// tougher blocks (see [`required_tier`]). Wood `1` < stone `2` < iron `3` <
/// tungsten `4`.
pub fn pickaxe_tier(item: BlockId) -> u8 {
    match item {
        PICKAXE => 1,
        STONE_PICKAXE => 2,
        IRON_PICKAXE => 3,
        TUNGSTEN_PICKAXE => 4,
        _ => 0,
    }
}

/// Minimum [`pickaxe_tier`] needed to mine `block` quickly and have it drop.
/// `0` means no pickaxe is required. A block can still be chipped at by hand or
/// with too weak a tool, but only very slowly and without yielding a drop.
pub fn required_tier(block: BlockId) -> u8 {
    match block {
        STONE => 1,        // any pickaxe
        STONE_BRICKS => 1, // any pickaxe — crafted stone, mined like stone
        CHARRED_ROCK => 1, // any pickaxe — the underworld's stone
        COAL_ORE => 1,     // any pickaxe
        IRON_ORE => 2,     // stone or iron pickaxe only
        GOLD_ORE => 2,     // stone pickaxe or better — soft but deep
        TUNGSTEN_ORE => 3, // iron pickaxe or better — the underworld's hardest ore
        _ => 0,
    }
}

/// Multiplier applied to a block's [`break_secs`](BlockDef::break_secs) given
/// the item the player is holding (`held`; [`AIR`] means bare hands). Smaller is
/// faster. Mining a tool-gated block with too weak a tool is punishingly slow;
/// the right tool (or better) speeds it up, more so at higher tiers.
pub fn mine_speed_mult(block: BlockId, held: BlockId) -> f32 {
    // An axe is the right tool for wood: it bites through logs quickly.
    if block == LOG && is_axe(held) {
        return 0.4;
    }
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
        3 => 0.18, // iron
        _ => 0.12, // tungsten (tier 4) and up: the fastest
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
        PICKAXE | WOOD_SWORD | WOOD_AXE => 60,
        STONE_PICKAXE | STONE_SWORD | STONE_AXE => 132,
        IRON_PICKAXE | IRON_SWORD | IRON_AXE => 251,
        TUNGSTEN_PICKAXE | TUNGSTEN_SWORD | TUNGSTEN_AXE => 480,
        // Armor is sturdier than a tool — it soaks many blows before it gives out
        // (one point per blow absorbed; see [`crate::server`]). Worn down, it can be
        // mended at a forge with its own metal, just like a tool.
        IRON_ARMOR => 300,
        TUNGSTEN_ARMOR => 600,
        _ => 0,
    }
}

/// Whether `block` can be climbed — a ladder the player clings to and moves up
/// and down with the jump/down inputs instead of falling through.
pub fn is_climbable(block: BlockId) -> bool {
    matches!(block, LADDER | ROPE_LADDER)
}

/// Whether `block` is a rope ladder — the climbable block that unrolls down a
/// shaft on placement and may be anchored from a solid block above.
pub fn is_rope_ladder(block: BlockId) -> bool {
    block == ROPE_LADDER
}

/// Whether `block` is any part of a door — either half, open or closed. A door
/// is two cells tall, so both halves carry this.
pub fn is_door(block: BlockId) -> bool {
    matches!(block, DOOR | DOOR_TOP | DOOR_OPEN | DOOR_OPEN_TOP)
}

/// Whether `block` is the *lower* half of a door (the cell a door is anchored to);
/// its matching upper half sits in the cell directly above.
pub fn is_door_bottom(block: BlockId) -> bool {
    matches!(block, DOOR | DOOR_OPEN)
}

/// Whether `block` is water — the fluid the player swims through instead of
/// walking on or mining.
pub fn is_water(block: BlockId) -> bool {
    block == WATER
}

/// Whether `block` is fire — the underworld hazard that burns anything standing
/// in it. Non-solid, so the player (and creatures) walk through it and take damage
/// rather than being blocked.
pub fn is_fire(block: BlockId) -> bool {
    block == FIRE
}

/// Whether `item` is a bucket in either state (empty or water-filled) — the
/// tool used to scoop and pour water.
pub fn is_bucket(item: BlockId) -> bool {
    matches!(item, BUCKET | WATER_BUCKET)
}

/// Whether `item` is the fire key — the artifact whose use warps the player
/// between dimensions.
pub fn is_fire_key(item: BlockId) -> bool {
    item == FIRE_KEY
}

/// Whether `item` is the arena key — the artifact whose use warps the player
/// into and out of the stone-brick boss arena.
pub fn is_arena_key(item: BlockId) -> bool {
    item == ARENA_KEY
}

/// Whether `item` is a boat — the vehicle ridden to glide across water.
pub fn is_boat(item: BlockId) -> bool {
    item == BOAT
}

/// Whether `item` is a musket — the ranged firearm fired (by right-click) toward the
/// cursor, spending a [`BULLET`] as ammunition rather than being placed or consumed.
pub fn is_musket(item: BlockId) -> bool {
    item == MUSKET
}

/// Mana spent casting the summoner spell once. Shared by the server (which charges
/// it) and the client (which previews the cost and gates the cast on having enough).
pub const SUMMONER_SPELL_MANA_COST: i32 = 25;

/// Mana spent casting the sunburst spell once. Costlier than the summoner spell — it
/// is a panic-button area-of-effect that wipes daylight-burning undead at a stroke.
pub const SUNBURST_SPELL_MANA_COST: i32 = 50;

/// Mana spent casting the restore spell once. The dearest of the spells — it turns a
/// deadly foe into a loyal ally (a knight or mage) at a stroke.
pub const RESTORE_SPELL_MANA_COST: i32 = 60;

/// Mana spent casting the dragonian steed spell once. The dearest cast in the world —
/// it summons a whole flying miniboss to fight at your side and carry you aloft.
pub const DRAGONIAN_STEED_MANA_COST: i32 = 80;

/// Mana spent casting the dragon plate spell once. The very dearest cast — a Twinscale
/// drop that wraps the caster in a ward stronger than any armor for a few minutes.
pub const DRAGON_PLATE_SPELL_MANA_COST: i32 = 100;

/// Seconds the [`DRAGON_PLATE_SPELL`] ward lasts after a cast — eight minutes — before
/// it lapses and the caster must recast to renew it. Shared by the server (which times
/// it down) and any code that previews the buff.
pub const DRAGON_PLATE_BUFF_DURATION: f32 = 8.0 * 60.0;

/// The *defense* (percentage of incoming damage turned aside) granted by an active
/// [`DRAGON_PLATE_SPELL`] ward — far higher than even [`TUNGSTEN_ARMOR`]'s `55`. While
/// the ward holds it replaces (does not stack with) any worn armor's
/// [`armor_defense`], and like armor a blow still always lands for at least 1.
pub const DRAGON_PLATE_DEFENSE: i32 = 85;

/// Melee damage an *empty hand* deals while an active [`DRAGON_PLATE_SPELL`] ward arms
/// it — stronger than even a [`TUNGSTEN_AXE`] (`20`), the hardest-hitting forged weapon.
/// Applies only to a bare-handed swing ([`AIR`]); a warded player holding a weapon still
/// swings that weapon's own [`attack_damage`].
pub const DRAGON_PLATE_FIST_DAMAGE: i32 = 30;

/// Whether `item` is a spellbook — a magic tome cast (by right-clicking) at the
/// cost of mana rather than placed or consumed.
pub fn is_spellbook(item: BlockId) -> bool {
    matches!(
        item,
        SUMMONER_SPELL | SUNBURST_SPELL | RESTORE_SPELL | DRAGONIAN_STEED | DRAGON_PLATE_SPELL
    )
}

/// The mana cost of casting the spellbook `item` once, or `None` if `item` is not a
/// spellbook. Casting a spellbook never consumes the book — only this much mana.
pub fn spell_mana_cost(item: BlockId) -> Option<i32> {
    match item {
        SUMMONER_SPELL => Some(SUMMONER_SPELL_MANA_COST),
        SUNBURST_SPELL => Some(SUNBURST_SPELL_MANA_COST),
        RESTORE_SPELL => Some(RESTORE_SPELL_MANA_COST),
        DRAGONIAN_STEED => Some(DRAGONIAN_STEED_MANA_COST),
        DRAGON_PLATE_SPELL => Some(DRAGON_PLATE_SPELL_MANA_COST),
        _ => None,
    }
}

/// Whether `item` is a pickaxe (mining is its intended use).
pub fn is_pickaxe(item: BlockId) -> bool {
    matches!(
        item,
        PICKAXE | STONE_PICKAXE | IRON_PICKAXE | TUNGSTEN_PICKAXE
    )
}

/// Whether `item` is a sword (attacking is its intended use).
pub fn is_sword(item: BlockId) -> bool {
    matches!(item, WOOD_SWORD | STONE_SWORD | IRON_SWORD | TUNGSTEN_SWORD)
}

/// Whether `item` is an axe (felling trees and fighting are its uses).
pub fn is_axe(item: BlockId) -> bool {
    matches!(item, WOOD_AXE | STONE_AXE | IRON_AXE | TUNGSTEN_AXE)
}

/// Durability spent attacking with `item`. A sword's intended use costs `1`; a
/// pickaxe swung as a weapon wears twice as fast. An axe also wears twice as
/// fast — its extra punch is paid for in durability. Anything else costs nothing.
pub fn attack_wear(item: BlockId) -> u16 {
    if is_sword(item) {
        1
    } else if is_pickaxe(item) || is_axe(item) {
        2
    } else {
        0
    }
}

/// Durability spent mining with `item`. A pickaxe's intended use costs `1`; a
/// sword used to dig wears twice as fast. An axe (whose job is chopping wood)
/// always wears twice as fast. Anything else costs nothing.
pub fn mine_wear(item: BlockId) -> u16 {
    if is_pickaxe(item) {
        1
    } else if is_sword(item) || is_axe(item) {
        2
    } else {
        0
    }
}

/// The material that repairs `item` at a [`FORGE`] — the same material it is
/// crafted from — or `None` if `item` has no durability to repair.
pub fn repair_material(item: BlockId) -> Option<BlockId> {
    match item {
        PICKAXE | WOOD_SWORD | WOOD_AXE => Some(WOOD),
        STONE_PICKAXE | STONE_SWORD | STONE_AXE => Some(STONE),
        IRON_PICKAXE | IRON_SWORD | IRON_AXE | IRON_ARMOR => Some(IRON_INGOT),
        TUNGSTEN_PICKAXE | TUNGSTEN_SWORD | TUNGSTEN_AXE | TUNGSTEN_ARMOR => Some(TUNGSTEN_INGOT),
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
        // Axes out-hit swords of the same tier, the trade-off being durability.
        WOOD_AXE => 10,
        STONE_AXE => 13,
        IRON_AXE => 16,
        TUNGSTEN_AXE => 20,
        WOOD_SWORD => 8,
        STONE_SWORD => 11,
        IRON_SWORD => 14,
        TUNGSTEN_SWORD => 18,
        PICKAXE => 4,
        STONE_PICKAXE => 5,
        IRON_PICKAXE => 6,
        TUNGSTEN_PICKAXE => 8,
        _ => 3, // bare hands or any non-weapon item
    }
}

/// The item a broken `block` drops (assuming [`drops_when_mined`]). Most blocks
/// drop themselves; a few transform (iron ore yields raw iron, a lit campfire
/// drops the plain campfire it reverts to). Leaves are special-cased separately
/// in [`mined_drop_rolled`] because their drop is randomized.
pub fn mined_drop(block: BlockId) -> BlockId {
    match block {
        IRON_ORE => RAW_IRON,
        TUNGSTEN_ORE => RAW_TUNGSTEN,
        GOLD_ORE => RAW_GOLD,
        COAL_ORE => COAL,
        CAMPFIRE_LIT => CAMPFIRE,
        // A broken locked chest reverts to a plain chest — the gold reinforcement
        // is lost, not recovered.
        LOCKED_CHEST => CHEST,
        // Whichever half of a door is struck, it drops one (closed) door item.
        DOOR_TOP | DOOR_OPEN | DOOR_OPEN_TOP => DOOR,
        other => other,
    }
}

/// The item a broken `block` drops given a random `roll` in `[0, 1)`, or `None`
/// for no drop. Leaves usually shed a stick, occasionally an apple, and sometimes
/// nothing; every other block drops deterministically (see [`mined_drop`]).
pub fn mined_drop_rolled(block: BlockId, roll: f32) -> Option<BlockId> {
    match block {
        LEAVES => {
            if roll < 0.70 {
                Some(STICK) // sticks are the common drop
            } else if roll < 0.85 {
                Some(APPLE) // apples are rarer than sticks
            } else {
                None // and sometimes leaves yield nothing
            }
        }
        // Fire isn't a material: breaking it just snuffs it out, dropping nothing.
        FIRE => None,
        other => Some(mined_drop(other)),
    }
}

// --- Food & cooking ------------------------------------------------------

/// Health restored by eating `item`, or `None` if it isn't food. A negative
/// value *costs* health: eating [`RAW_MEAT`] makes you sick, so it must be
/// cooked into [`COOKED_MEAT`] on a [`CAMPFIRE`] first.
pub fn food_heal(item: BlockId) -> Option<i32> {
    match item {
        APPLE => Some(4),
        COOKED_MEAT => Some(8),
        RAW_MEAT => Some(-3),
        _ => None,
    }
}

/// Whether `item` can be eaten (has a [`food_heal`] effect).
pub fn is_food(item: BlockId) -> bool {
    food_heal(item).is_some()
}

/// Whether `block` is a campfire in either state (unlit or lit).
pub fn is_campfire(block: BlockId) -> bool {
    matches!(block, CAMPFIRE | CAMPFIRE_LIT)
}

/// Whether `block` is a [`SIGN`].
pub fn is_sign(block: BlockId) -> bool {
    block == SIGN
}

/// Whether `block` is a [`QUEST_BOARD`].
pub fn is_quest_board(block: BlockId) -> bool {
    block == QUEST_BOARD
}

/// Whether `block` carries player-written text — a sign or a quest board — so it
/// has an associated [`BlockText`](crate::protocol::BlockText) entry.
pub fn is_text_block(block: BlockId) -> bool {
    is_sign(block) || is_quest_board(block)
}

/// Whether `block` is a plain (unlocked) [`CHEST`].
pub fn is_chest(block: BlockId) -> bool {
    block == CHEST
}

/// Whether `block` is a [`LOCKED_CHEST`].
pub fn is_locked_chest(block: BlockId) -> bool {
    block == LOCKED_CHEST
}

/// Whether `block` is storage — a chest in either state — so it has an associated
/// per-cell slot store (see [`crate::server`]).
pub fn is_any_chest(block: BlockId) -> bool {
    is_chest(block) || is_locked_chest(block)
}

/// Seconds of burn time one unit of `item` adds to a campfire when used as fuel,
/// or `None` if it isn't fuel. Coal burns longest, wood burns long, and bark
/// gives a smaller boost.
pub fn fuel_seconds(item: BlockId) -> Option<f32> {
    match item {
        COAL => Some(90.0),
        WOOD => Some(45.0),
        BARK => Some(12.0),
        _ => None,
    }
}

/// The fuels a [`FORGE`] accepts, in the order its GUI offers them. Every entry
/// has a [`forge_fuel_units`] charge cost.
pub const FORGE_FUELS: &[BlockId] = &[WOOD, COAL, BARK];

/// How many units of `item` one smelt at a [`FORGE`] consumes when `item` is used
/// as its fuel, or `None` if `item` can't fuel a forge. Coal and wood each spend a
/// single unit; bark is weaker, so it takes four to fire one smelt.
pub fn forge_fuel_units(item: BlockId) -> Option<u32> {
    match item {
        COAL | WOOD => Some(1),
        BARK => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_roll_stick_apple_or_nothing() {
        // Sticks are the common drop, apples rarer, and the tail is empty.
        assert_eq!(mined_drop_rolled(LEAVES, 0.0), Some(STICK));
        assert_eq!(mined_drop_rolled(LEAVES, 0.69), Some(STICK));
        assert_eq!(mined_drop_rolled(LEAVES, 0.70), Some(APPLE));
        assert_eq!(mined_drop_rolled(LEAVES, 0.84), Some(APPLE));
        assert_eq!(mined_drop_rolled(LEAVES, 0.85), None);
        assert_eq!(mined_drop_rolled(LEAVES, 0.99), None);
        // Apples must stay rarer than sticks (smaller probability band).
        let stick_band = 0.70;
        let apple_band = 0.85 - 0.70;
        assert!(apple_band < stick_band);
    }

    #[test]
    fn other_blocks_drop_deterministically_regardless_of_roll() {
        for roll in [0.0, 0.5, 0.999] {
            assert_eq!(mined_drop_rolled(STONE, roll), Some(STONE));
            assert_eq!(mined_drop_rolled(IRON_ORE, roll), Some(RAW_IRON));
            assert_eq!(mined_drop_rolled(CAMPFIRE_LIT, roll), Some(CAMPFIRE));
        }
    }

    #[test]
    fn axes_out_hit_swords_but_wear_twice_as_fast() {
        // Each axe hits harder than the sword of its tier.
        assert!(attack_damage(WOOD_AXE) > attack_damage(WOOD_SWORD));
        assert!(attack_damage(STONE_AXE) > attack_damage(STONE_SWORD));
        assert!(attack_damage(IRON_AXE) > attack_damage(IRON_SWORD));
        // An axe wears twice as fast as a sword, both attacking and mining.
        for axe in [WOOD_AXE, STONE_AXE, IRON_AXE] {
            assert!(is_axe(axe));
            assert_eq!(attack_wear(axe), 2);
            assert_eq!(mine_wear(axe), 2);
            assert_eq!(attack_wear(axe), 2 * attack_wear(WOOD_SWORD));
            assert!(max_durability(axe) > 0);
            assert_eq!(max_stack(axe), 1);
            assert!(repair_material(axe).is_some());
        }
        // An axe is the fast tool for logs; bare hands aren't.
        assert!(mine_speed_mult(LOG, IRON_AXE) < mine_speed_mult(LOG, AIR));
    }

    #[test]
    fn tungsten_outclasses_iron_across_every_tool() {
        // Tungsten is the new top tier: it mines faster, lasts longer, and hits
        // harder than iron in every tool family.
        assert!(pickaxe_tier(TUNGSTEN_PICKAXE) > pickaxe_tier(IRON_PICKAXE));
        assert!(mine_speed_mult(STONE, TUNGSTEN_PICKAXE) < mine_speed_mult(STONE, IRON_PICKAXE));
        assert!(max_durability(TUNGSTEN_PICKAXE) > max_durability(IRON_PICKAXE));
        assert!(attack_damage(TUNGSTEN_SWORD) > attack_damage(IRON_SWORD));
        assert!(attack_damage(TUNGSTEN_AXE) > attack_damage(IRON_AXE));
        assert!(attack_damage(TUNGSTEN_PICKAXE) > attack_damage(IRON_PICKAXE));
        // Each tungsten tool behaves like a tool: stacks to one, has durability,
        // and is mended at a forge with tungsten ingots.
        for t in [TUNGSTEN_PICKAXE, TUNGSTEN_SWORD, TUNGSTEN_AXE] {
            assert_eq!(max_stack(t), 1);
            assert!(max_durability(t) > 0);
            assert_eq!(repair_material(t), Some(TUNGSTEN_INGOT));
        }
        // Tungsten ore is the underworld's hardest dig: only an iron pickaxe or
        // better frees it, and it yields raw tungsten when it does.
        assert_eq!(required_tier(TUNGSTEN_ORE), 3);
        assert!(!drops_when_mined(TUNGSTEN_ORE, STONE_PICKAXE));
        assert!(drops_when_mined(TUNGSTEN_ORE, IRON_PICKAXE));
        assert_eq!(mined_drop(TUNGSTEN_ORE), RAW_TUNGSTEN);
    }

    #[test]
    fn rope_ladders_are_climbable_placeable_and_rope_is_not() {
        let reg = BlockRegistry::new();
        // Both ladder kinds can be climbed; rope (the material) cannot.
        assert!(is_climbable(LADDER));
        assert!(is_climbable(ROPE_LADDER));
        assert!(is_rope_ladder(ROPE_LADDER));
        assert!(!is_rope_ladder(LADDER));
        assert!(!is_climbable(ROPE));
        // A rope ladder is a placeable, non-solid block; rope is a plain item.
        assert!(reg.is_placeable(ROPE_LADDER));
        assert!(!reg.is_solid(ROPE_LADDER));
        assert!(!reg.is_placeable(ROPE));
    }

    #[test]
    fn water_is_a_nonsolid_unplaceable_fluid() {
        let reg = BlockRegistry::new();
        assert!(is_water(WATER));
        assert!(!is_water(STONE));
        // Swum through (non-solid) and neither placed nor (by hand) mined.
        assert!(!reg.is_solid(WATER));
        assert!(!reg.is_placeable(WATER));
        // Visible so a body of water draws, but it carries no breaking delay.
        assert!(reg.get(WATER).visible);
        assert_eq!(reg.get(WATER).break_secs, 0.0);
    }

    #[test]
    fn buckets_are_unplaceable_items_and_only_the_full_one_is_unique() {
        let reg = BlockRegistry::new();
        assert!(is_bucket(BUCKET));
        assert!(is_bucket(WATER_BUCKET));
        assert!(!is_bucket(IRON_INGOT));
        // Buckets are plain items: visible (for the inventory icon) but never
        // placed into the world as blocks.
        for b in [BUCKET, WATER_BUCKET] {
            assert!(reg.get(b).visible);
            assert!(!reg.is_placeable(b));
        }
        // A water bucket carries one load (no stacking); an empty one stacks.
        assert_eq!(max_stack(WATER_BUCKET), 1);
        assert_eq!(max_stack(BUCKET), crate::inventory::STACK_MAX);
    }

    #[test]
    fn food_heals_but_raw_meat_hurts() {
        assert_eq!(food_heal(APPLE), Some(4));
        assert_eq!(food_heal(COOKED_MEAT), Some(8));
        // Raw meat is food, but eating it costs health.
        assert_eq!(food_heal(RAW_MEAT), Some(-3));
        assert!(is_food(RAW_MEAT));
        // Cooking turns the harmful raw meat into a beneficial meal.
        assert!(food_heal(RAW_MEAT).unwrap() < 0);
        assert!(food_heal(COOKED_MEAT).unwrap() > 0);
        // Non-food items can't be eaten.
        assert_eq!(food_heal(STONE), None);
        assert!(!is_food(STICK));
    }

    #[test]
    fn coal_wood_and_bark_are_fuel_and_coal_burns_longest() {
        assert!(fuel_seconds(COAL).unwrap() > fuel_seconds(WOOD).unwrap());
        assert!(fuel_seconds(WOOD).unwrap() > fuel_seconds(BARK).unwrap());
        assert_eq!(fuel_seconds(STONE), None);
        assert_eq!(fuel_seconds(APPLE), None);
    }

    #[test]
    fn forge_burns_wood_coal_or_bark_with_bark_the_priciest() {
        // Wood and coal each fire a smelt for one unit; weak bark takes four.
        assert_eq!(forge_fuel_units(WOOD), Some(1));
        assert_eq!(forge_fuel_units(COAL), Some(1));
        assert_eq!(forge_fuel_units(BARK), Some(4));
        assert_eq!(forge_fuel_units(STONE), None);
        // Every offered fuel must actually be a fuel.
        assert!(FORGE_FUELS.iter().all(|f| forge_fuel_units(*f).is_some()));
    }

    #[test]
    fn summoner_spell_is_a_castable_unstacking_spellbook() {
        let reg = BlockRegistry::new();
        // The spellbook is a recognized spellbook with a mana cost; ordinary items
        // are not and have no cost.
        assert!(is_spellbook(SUMMONER_SPELL));
        assert!(!is_spellbook(STONE));
        assert_eq!(
            spell_mana_cost(SUMMONER_SPELL),
            Some(SUMMONER_SPELL_MANA_COST)
        );
        assert_eq!(spell_mana_cost(STONE), None);
        // The sunburst and restore spells are spellbooks too, each costlier than the
        // last — and every spellbook has a mana cost.
        for (book, cost) in [
            (SUNBURST_SPELL, SUNBURST_SPELL_MANA_COST),
            (RESTORE_SPELL, RESTORE_SPELL_MANA_COST),
            (DRAGONIAN_STEED, DRAGONIAN_STEED_MANA_COST),
            (DRAGON_PLATE_SPELL, DRAGON_PLATE_SPELL_MANA_COST),
        ] {
            assert!(is_spellbook(book));
            assert_eq!(spell_mana_cost(book), Some(cost));
        }
        assert!(SUNBURST_SPELL_MANA_COST > SUMMONER_SPELL_MANA_COST);
        assert!(RESTORE_SPELL_MANA_COST > SUNBURST_SPELL_MANA_COST);
        assert!(DRAGONIAN_STEED_MANA_COST > RESTORE_SPELL_MANA_COST);
        assert!(DRAGON_PLATE_SPELL_MANA_COST > DRAGONIAN_STEED_MANA_COST);
        // The dragon plate ward is far stronger than the sturdiest forged armor.
        assert!(DRAGON_PLATE_DEFENSE > armor_defense(TUNGSTEN_ARMOR));
        // A single bound tome: each never stacks, and is a plain (non-placeable)
        // item with an atlas tile for its inventory/dropped sprite.
        for book in [
            SUMMONER_SPELL,
            SUNBURST_SPELL,
            RESTORE_SPELL,
            DRAGONIAN_STEED,
            DRAGON_PLATE_SPELL,
        ] {
            assert_eq!(max_stack(book), 1);
            assert!(reg.get(book).visible);
            assert!(!reg.is_placeable(book));
        }
    }

    #[test]
    fn coal_ore_drops_coal_when_mined() {
        assert_eq!(mined_drop(COAL_ORE), COAL);
    }

    #[test]
    fn campfire_states_are_recognized() {
        assert!(is_campfire(CAMPFIRE));
        assert!(is_campfire(CAMPFIRE_LIT));
        assert!(!is_campfire(FORGE));
    }

    #[test]
    fn doors_are_two_tall_and_swing_between_solid_and_passable() {
        let reg = BlockRegistry::new();
        // Every half of a door is recognized as a door.
        for d in [DOOR, DOOR_TOP, DOOR_OPEN, DOOR_OPEN_TOP] {
            assert!(is_door(d));
        }
        assert!(!is_door(STONE));
        // Only the open/closed lower cells anchor a door; the tops do not.
        assert!(is_door_bottom(DOOR));
        assert!(is_door_bottom(DOOR_OPEN));
        assert!(!is_door_bottom(DOOR_TOP));
        assert!(!is_door_bottom(DOOR_OPEN_TOP));
        // Closed halves block movement; open halves are stepped through.
        assert!(reg.is_solid(DOOR));
        assert!(reg.is_solid(DOOR_TOP));
        assert!(!reg.is_solid(DOOR_OPEN));
        assert!(!reg.is_solid(DOOR_OPEN_TOP));
        // Only the closed lower half is held and placed; the rest are world-only.
        assert!(reg.is_placeable(DOOR));
        assert!(!reg.is_placeable(DOOR_TOP));
        assert!(!reg.is_placeable(DOOR_OPEN));
        assert!(!reg.is_placeable(DOOR_OPEN_TOP));
        // Breaking any half drops a single closed door.
        for d in [DOOR, DOOR_TOP, DOOR_OPEN, DOOR_OPEN_TOP] {
            assert_eq!(mined_drop(d), DOOR);
        }
    }
}
