//! Crafting recipes, shared by client and server.
//!
//! Every recipe is *unshaped* (shapeless): it consumes a set of input items in
//! any arrangement and yields a set of outputs, both of which may name multiple
//! item types and quantities. The client lists recipes on the inventory screen
//! and the server validates and executes them authoritatively (see
//! [`crate::server`] and [`ClientMessage::Craft`](crate::protocol::ClientMessage::Craft)).
//!
//! A recipe is identified on the wire by its index in [`RECIPES`].

use crate::block::{BARK, LOG, PICKAXE, STICK, STONE, STONE_PICKAXE, WOOD};
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
];

impl Recipe {
    /// Whether `inv` holds enough of every input to craft this recipe once.
    pub fn craftable(&self, inv: &Inventory) -> bool {
        self.inputs.iter().all(|(item, n)| inv.count(*item) >= *n)
    }
}
