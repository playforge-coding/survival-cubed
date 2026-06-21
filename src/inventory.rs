//! Slot-based player inventory, shared by client and server.
//!
//! An inventory is a flat list of [`TOTAL_SLOTS`] slots: the first
//! [`HOTBAR_SLOTS`] are the hotbar (selectable with the number keys and used for
//! placing), the remaining [`STORAGE_SLOTS`] are backpack storage shown only on
//! the inventory screen. Each slot is either empty or a stack of one block type,
//! capped at [`STACK_MAX`].
//!
//! The server owns the authoritative copy; the client mirrors it for display and
//! optimistic prediction. The same type round-trips over the wire (see
//! [`crate::protocol::ServerMessage::Inventory`]) and to disk (see
//! [`crate::save::SavedPlayer`]).

use serde::{Deserialize, Serialize};

use crate::block::max_stack;
use crate::protocol::BlockId;

/// Number of hotbar slots (selectable with keys 1–9, used for placing).
pub const HOTBAR_SLOTS: usize = 9;
/// Number of backpack storage slots (visible on the inventory screen).
pub const STORAGE_SLOTS: usize = 27;
/// Total slots in an inventory: hotbar followed by storage.
pub const TOTAL_SLOTS: usize = HOTBAR_SLOTS + STORAGE_SLOTS;
/// Maximum number of identical blocks that fit in one slot.
pub const STACK_MAX: u32 = 64;

/// One inventory slot: a `(block, count)` stack with `1 <= count <= STACK_MAX`,
/// or `None` when empty.
pub type Slot = Option<(BlockId, u32)>;

/// A player's full inventory: a fixed-length list of [`Slot`]s.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inventory {
    slots: Vec<Slot>,
}

impl Default for Inventory {
    fn default() -> Self {
        Self::new()
    }
}

impl Inventory {
    /// An empty inventory of the standard size.
    pub fn new() -> Self {
        Inventory {
            slots: vec![None; TOTAL_SLOTS],
        }
    }

    /// Build from a wire/disk slot list, normalizing to exactly [`TOTAL_SLOTS`]
    /// (padding with empties or truncating) so corrupt or outdated data is safe.
    pub fn from_slots(mut slots: Vec<Slot>) -> Self {
        slots.resize(TOTAL_SLOTS, None);
        Inventory { slots }
    }

    /// The slots, hotbar first then storage.
    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// A snapshot of the slots for the wire or disk.
    pub fn to_slots(&self) -> Vec<Slot> {
        self.slots.clone()
    }

    /// Contents of `slot`, or `None` if empty or out of range.
    pub fn get(&self, slot: usize) -> Slot {
        self.slots.get(slot).copied().flatten()
    }

    /// Add `count` of `block`, topping up existing matching stacks first (in slot
    /// order) and then filling empty slots. Returns the amount that did *not*
    /// fit (0 when everything was stored).
    pub fn add(&mut self, block: BlockId, mut count: u32) -> u32 {
        let cap = max_stack(block);
        for slot in self.slots.iter_mut() {
            if count == 0 {
                break;
            }
            if let Some((b, n)) = slot
                && *b == block
                && *n < cap
            {
                let room = cap - *n;
                let moved = room.min(count);
                *n += moved;
                count -= moved;
            }
        }
        for slot in self.slots.iter_mut() {
            if count == 0 {
                break;
            }
            if slot.is_none() {
                let moved = count.min(cap);
                *slot = Some((block, moved));
                count -= moved;
            }
        }
        count
    }

    /// Total number of `item` held across every slot.
    pub fn count(&self, item: BlockId) -> u32 {
        self.slots
            .iter()
            .filter_map(|s| *s)
            .filter(|(b, _)| *b == item)
            .map(|(_, n)| n)
            .sum()
    }

    /// Remove `count` of `item`, drawing from matching stacks in slot order.
    /// Removes nothing and returns `false` if fewer than `count` are held.
    pub fn remove(&mut self, item: BlockId, mut count: u32) -> bool {
        if self.count(item) < count {
            return false;
        }
        for slot in self.slots.iter_mut() {
            if count == 0 {
                break;
            }
            if let Some((b, n)) = slot
                && *b == item
            {
                let taken = (*n).min(count);
                *n -= taken;
                count -= taken;
                if *n == 0 {
                    *slot = None;
                }
            }
        }
        true
    }

    /// Remove one item from `slot`, returning its block id (or `None` if the slot
    /// was empty / out of range). Empties the slot when its last item is taken.
    pub fn take_one(&mut self, slot: usize) -> Option<BlockId> {
        let s = self.slots.get_mut(slot)?;
        let (block, n) = s.as_mut()?;
        let block = *block;
        *n -= 1;
        if *n == 0 {
            *s = None;
        }
        Some(block)
    }

    /// Move the stack in `from` onto `to`. Same-block stacks merge up to
    /// [`STACK_MAX`] (any remainder stays in `from`); an empty `to` receives the
    /// whole stack; otherwise the two slots swap. No-op for equal/out-of-range
    /// indices or an empty source.
    pub fn move_stack(&mut self, from: usize, to: usize) {
        if from == to || from >= self.slots.len() || to >= self.slots.len() {
            return;
        }
        let Some((fb, fcount)) = self.slots[from] else {
            return;
        };
        match self.slots[to] {
            None => {
                self.slots[to] = Some((fb, fcount));
                self.slots[from] = None;
            }
            Some((tb, tcount)) if tb == fb => {
                let room = max_stack(tb).saturating_sub(tcount);
                let moved = room.min(fcount);
                self.slots[to] = Some((tb, tcount + moved));
                let left = fcount - moved;
                self.slots[from] = if left == 0 { None } else { Some((fb, left)) };
            }
            Some(_) => self.slots.swap(from, to),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STONE: BlockId = 1;
    const DIRT: BlockId = 2;
    const PICKAXE: BlockId = crate::block::PICKAXE;

    #[test]
    fn tools_stack_to_one_per_slot() {
        let mut inv = Inventory::new();
        // Three pickaxes can't merge; each takes its own slot.
        assert_eq!(inv.add(PICKAXE, 3), 0);
        assert_eq!(inv.get(0), Some((PICKAXE, 1)));
        assert_eq!(inv.get(1), Some((PICKAXE, 1)));
        assert_eq!(inv.get(2), Some((PICKAXE, 1)));
        assert_eq!(inv.count(PICKAXE), 3);
    }

    #[test]
    fn remove_draws_across_stacks_or_fails() {
        let mut inv = Inventory::new();
        inv.add(STONE, 70); // 64 + 6 across two slots
        assert!(!inv.remove(STONE, 80)); // not enough: no-op
        assert_eq!(inv.count(STONE), 70);
        assert!(inv.remove(STONE, 66)); // drains slot 0, dips into slot 1
        assert_eq!(inv.count(STONE), 4);
        assert_eq!(inv.get(0), None);
        assert_eq!(inv.get(1), Some((STONE, 4)));
    }

    #[test]
    fn add_stacks_then_fills_empty_slots() {
        let mut inv = Inventory::new();
        assert_eq!(inv.add(STONE, 70), 0);
        // 70 stone: 64 in slot 0, 6 in slot 1.
        assert_eq!(inv.get(0), Some((STONE, 64)));
        assert_eq!(inv.get(1), Some((STONE, 6)));
        // Topping up fills slot 1 to 64 before opening a new slot.
        assert_eq!(inv.add(STONE, 60), 0);
        assert_eq!(inv.get(1), Some((STONE, 64)));
        assert_eq!(inv.get(2), Some((STONE, 2)));
    }

    #[test]
    fn add_reports_overflow_when_full() {
        let mut inv = Inventory::new();
        let total = STACK_MAX * TOTAL_SLOTS as u32;
        assert_eq!(inv.add(STONE, total), 0);
        assert_eq!(inv.add(STONE, 5), 5); // full: nothing fits
    }

    #[test]
    fn move_merges_swaps_and_relocates() {
        let mut inv = Inventory::new();
        inv.add(STONE, 50);
        // Relocate into an empty slot.
        inv.move_stack(0, 10);
        assert_eq!(inv.get(0), None);
        assert_eq!(inv.get(10), Some((STONE, 50)));
        // Merge with cap, leaving a remainder behind.
        inv.add(STONE, 30); // lands in slot 0
        inv.move_stack(0, 10);
        assert_eq!(inv.get(10), Some((STONE, 64)));
        assert_eq!(inv.get(0), Some((STONE, 16)));
        // Swap different blocks.
        inv.add(DIRT, 5); // slot 1
        inv.move_stack(0, 1);
        assert_eq!(inv.get(0), Some((DIRT, 5)));
        assert_eq!(inv.get(1), Some((STONE, 16)));
    }

    #[test]
    fn take_one_empties_slot_at_zero() {
        let mut inv = Inventory::new();
        inv.add(DIRT, 1);
        assert_eq!(inv.take_one(0), Some(DIRT));
        assert_eq!(inv.get(0), None);
        assert_eq!(inv.take_one(0), None);
    }
}
