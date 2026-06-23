//! Crafting recipes, shared by client and server.
//!
//! Every recipe is *unshaped* (shapeless): it consumes a set of input items in
//! any arrangement and yields a set of outputs, both of which may name multiple
//! item types and quantities. The client lists recipes on the inventory screen
//! and the server validates and executes them authoritatively (see
//! [`crate::server`] and [`ClientMessage::Craft`](crate::protocol::ClientMessage::Craft)).
//!
//! A recipe is identified on the wire by its index in [`RECIPES`].

use crate::block::{
    BARK, BUCKET, CAMPFIRE, CHARRED_ROCK, COOKED_MEAT, DOOR, FIRE_KEY, FORGE, IRON_AXE, IRON_INGOT,
    IRON_PICKAXE, IRON_SWORD, LADDER, LOG, PICKAXE, RAW_IRON, RAW_MEAT, RAW_TUNGSTEN, ROPE,
    ROPE_LADDER, STICK, STONE, STONE_AXE, STONE_PICKAXE, STONE_SWORD, TUNGSTEN_AXE, TUNGSTEN_INGOT,
    TUNGSTEN_PICKAXE, TUNGSTEN_SWORD, WOOD, WOOD_AXE, WOOD_SWORD,
};
use crate::inventory::Inventory;
use crate::protocol::BlockId;

/// One shapeless crafting recipe: `inputs` consumed, `outputs` produced.
pub struct Recipe {
    /// Player-facing name, shown on the crafting panel.
    pub name: &'static str,
    /// Items consumed, as `(item, count)` pairs.
    pub inputs: &'static [(BlockId, u32)],
    /// Items produced, as `(item, count)` pairs.
    pub outputs: &'static [(BlockId, u32)],
}

/// All known recipes. A recipe's index here is its wire id.
pub const RECIPES: &[Recipe] = &[
    // A log splits into one plank of wood plus four strips of bark.
    Recipe {
        name: "Wood + Bark",
        inputs: &[(LOG, 1)],
        outputs: &[(WOOD, 1), (BARK, 4)],
    },
    // Sticks (dropped by leaves) bind into a wooden pickaxe.
    Recipe {
        name: "Wooden Pickaxe",
        inputs: &[(STICK, 3)],
        outputs: &[(PICKAXE, 1)],
    },
    // Mined stone lashed to sticks makes a sturdier pickaxe.
    Recipe {
        name: "Stone Pickaxe",
        inputs: &[(STONE, 3), (STICK, 2)],
        outputs: &[(STONE_PICKAXE, 1)],
    },
    // Iron ingots bound to sticks make the fastest pickaxe.
    Recipe {
        name: "Iron Pickaxe",
        inputs: &[(IRON_INGOT, 3), (STICK, 2)],
        outputs: &[(IRON_PICKAXE, 1)],
    },
    // Tungsten ingots bound to sticks: the strongest, fastest pickaxe of all.
    Recipe {
        name: "Tungsten Pickaxe",
        inputs: &[(TUNGSTEN_INGOT, 3), (STICK, 2)],
        outputs: &[(TUNGSTEN_PICKAXE, 1)],
    },
    // A plank edge bound to a stick makes a crude wooden sword.
    Recipe {
        name: "Wood Sword",
        inputs: &[(WOOD, 2), (STICK, 1)],
        outputs: &[(WOOD_SWORD, 1)],
    },
    // Sharpened stone on a stick: a sturdier blade.
    Recipe {
        name: "Stone Sword",
        inputs: &[(STONE, 2), (STICK, 1)],
        outputs: &[(STONE_SWORD, 1)],
    },
    // Forged iron on a stick: the deadliest blade.
    Recipe {
        name: "Iron Sword",
        inputs: &[(IRON_INGOT, 2), (STICK, 1)],
        outputs: &[(IRON_SWORD, 1)],
    },
    // Forged tungsten on a stick: the deadliest blade in the world.
    Recipe {
        name: "Tungsten Sword",
        inputs: &[(TUNGSTEN_INGOT, 2), (STICK, 1)],
        outputs: &[(TUNGSTEN_SWORD, 1)],
    },
    // Stacked stone builds a forge for smelting.
    Recipe {
        name: "Forge",
        inputs: &[(STONE, 8)],
        outputs: &[(FORGE, 1)],
    },
    // Planks and sticks lash together into a run of climbable ladders.
    Recipe {
        name: "Ladder",
        inputs: &[(WOOD, 1), (STICK, 2)],
        outputs: &[(LADDER, 3)],
    },
    // A ring of stone cradling a pile of bark: a campfire to cook on.
    Recipe {
        name: "Campfire",
        inputs: &[(STONE, 1), (BARK, 5)],
        outputs: &[(CAMPFIRE, 1)],
    },
    // Planks and sticks bound into a heavy wood-felling axe.
    Recipe {
        name: "Wood Axe",
        inputs: &[(WOOD, 3), (STICK, 2)],
        outputs: &[(WOOD_AXE, 1)],
    },
    // Stone heads make a sturdier axe.
    Recipe {
        name: "Stone Axe",
        inputs: &[(STONE, 3), (STICK, 2)],
        outputs: &[(STONE_AXE, 1)],
    },
    // Forged iron makes the deadliest axe.
    Recipe {
        name: "Iron Axe",
        inputs: &[(IRON_INGOT, 3), (STICK, 2)],
        outputs: &[(IRON_AXE, 1)],
    },
    // Forged tungsten makes the deadliest axe in the world.
    Recipe {
        name: "Tungsten Axe",
        inputs: &[(TUNGSTEN_INGOT, 3), (STICK, 2)],
        outputs: &[(TUNGSTEN_AXE, 1)],
    },
    // Strips of bark twist into a length of rope.
    Recipe {
        name: "Rope",
        inputs: &[(BARK, 2)],
        outputs: &[(ROPE, 1)],
    },
    // Coils of rope knot into a rope ladder for descending caves.
    Recipe {
        name: "Rope Ladder",
        inputs: &[(ROPE, 3)],
        outputs: &[(ROPE_LADDER, 1)],
    },
    // Beaten iron ingots fold into a watertight bucket for carrying water.
    Recipe {
        name: "Bucket",
        inputs: &[(IRON_INGOT, 3)],
        outputs: &[(BUCKET, 1)],
    },
    // Planks hewn and hung into a two-tall door that swings open and shut.
    Recipe {
        name: "Door",
        inputs: &[(WOOD, 6)],
        outputs: &[(DOOR, 1)],
    },
    // A shard of charred rock fused to a tungsten ingot forms the fire key, the
    // artifact that warps its bearer between dimensions.
    Recipe {
        name: "Fire Key",
        inputs: &[(CHARRED_ROCK, 1), (TUNGSTEN_INGOT, 1)],
        outputs: &[(FIRE_KEY, 1)],
    },
];

/// Cooking recipes, available only at a lit [`CAMPFIRE`](crate::block::CAMPFIRE)
/// (its GUI lists these). Unlike smelting, cooking burns no fuel of its own — the
/// campfire simply has to be lit. A recipe's index here is its wire id.
pub const COOK_RECIPES: &[Recipe] = &[
    // Raw meat sizzles into a safe, hearty cooked meal.
    Recipe {
        name: "Cooked Meat",
        inputs: &[(RAW_MEAT, 1)],
        outputs: &[(COOKED_MEAT, 1)],
    },
];

/// Smelting recipes, available only at a [`FORGE`](crate::block::FORGE) (its GUI
/// lists these; the plain crafting panel lists [`RECIPES`]). Each lists only its
/// raw input and output; the forge burns a separately-chosen fuel on top of these
/// (see [`forge_fuel_units`](crate::block::forge_fuel_units)). A recipe's index
/// here is its wire id.
pub const SMELT_RECIPES: &[Recipe] = &[
    // Raw iron smelts into a refined ingot (one charge of fuel per smelt).
    Recipe {
        name: "Iron Ingot",
        inputs: &[(RAW_IRON, 1)],
        outputs: &[(IRON_INGOT, 1)],
    },
    // Raw tungsten smelts into a refined ingot (one charge of fuel per smelt).
    Recipe {
        name: "Tungsten Ingot",
        inputs: &[(RAW_TUNGSTEN, 1)],
        outputs: &[(TUNGSTEN_INGOT, 1)],
    },
];

impl Recipe {
    /// Whether `inv` holds enough of every input to craft this recipe once.
    pub fn craftable(&self, inv: &Inventory) -> bool {
        self.inputs.iter().all(|(item, n)| inv.count(*item) >= *n)
    }

    /// How many times this recipe could be crafted back-to-back from `inv`,
    /// limited by the scarcest input. Used by the forge's "All" button.
    pub fn max_crafts(&self, inv: &Inventory) -> u32 {
        self.inputs
            .iter()
            .map(|(item, n)| inv.count(*item) / n)
            .min()
            .unwrap_or(0)
    }
}
