//! Chunked tile world shared by client and server.
//!
//! The world is infinite horizontally and fixed height vertically. Positive
//! `y` points *down* (row 0 is the sky ceiling), matching screen space. Blocks
//! are addressed by integer cell coordinates; chunks group `CHUNK_SIZE` cells
//! per side.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::block::{AIR, WATER};
use crate::protocol::BlockId;

/// A self-contained world layer with its own chunks, terrain, and creatures, all
/// living in the same save. The player digs to the bottom of the [`Overworld`]
/// (the surface) and falls through into the [`Underworld`], a fiery expanse of
/// charred rock; digging back up to its ceiling returns them to the surface.
///
/// Each dimension is generated and stored independently (see [`crate::worldgen`]
/// and [`crate::save`]); a connection only ever interacts with the dimension it
/// is currently in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Dimension {
    /// The surface world: grass, forests, mountains, caves.
    #[default]
    Overworld,
    /// The world beneath the overworld: charred rock, fire, and charred skeletons.
    Underworld,
}

impl Dimension {
    /// Every dimension, in id order. Used to iterate per-dimension state.
    pub const ALL: [Dimension; 2] = [Dimension::Overworld, Dimension::Underworld];

    /// Stable index in `[0, NUM_DIMENSIONS)`, for arrays keyed by dimension.
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Number of distinct [`Dimension`]s.
pub const NUM_DIMENSIONS: usize = Dimension::ALL.len();

/// Cells per chunk side.
pub const CHUNK_SIZE: i32 = 16;
/// Cells per chunk.
pub const CHUNK_AREA: usize = (CHUNK_SIZE * CHUNK_SIZE) as usize;
/// World height measured in chunks.
pub const WORLD_HEIGHT_CHUNKS: i32 = 16;
/// World height measured in cells.
pub const WORLD_HEIGHT: i32 = CHUNK_SIZE * WORLD_HEIGHT_CHUNKS;
/// Pixel size of one block on the world grid (before camera zoom).
pub const TILE_SIZE: f32 = 16.0;
/// Greatest distance (in cells) spreading water flows horizontally from a source
/// before it stops. Falling water resets this budget at each level, so a flow can
/// still travel farther by stepping down terrain (water runs downhill).
pub const WATER_SPREAD_MAX: u8 = 6;

/// Integer chunk coordinate.
pub type ChunkCoord = (i32, i32);

/// A square block of cells, stored row-major.
#[derive(Clone)]
pub struct Chunk {
    pub blocks: Box<[BlockId; CHUNK_AREA]>,
}

impl Chunk {
    pub fn empty() -> Self {
        Chunk {
            blocks: Box::new([AIR; CHUNK_AREA]),
        }
    }

    pub fn from_vec(v: Vec<BlockId>) -> Self {
        let mut blocks = Box::new([AIR; CHUNK_AREA]);
        for (i, b) in v.into_iter().take(CHUNK_AREA).enumerate() {
            blocks[i] = b;
        }
        Chunk { blocks }
    }

    #[inline]
    fn index(lx: i32, ly: i32) -> usize {
        (ly * CHUNK_SIZE + lx) as usize
    }

    #[inline]
    pub fn get(&self, lx: i32, ly: i32) -> BlockId {
        self.blocks[Self::index(lx, ly)]
    }

    #[inline]
    pub fn set(&mut self, lx: i32, ly: i32, b: BlockId) {
        self.blocks[Self::index(lx, ly)] = b;
    }
}

/// Convert a world cell to `(chunk coord, local coord)`.
#[inline]
pub fn to_chunk(x: i32, y: i32) -> (ChunkCoord, (i32, i32)) {
    let cx = x.div_euclid(CHUNK_SIZE);
    let cy = y.div_euclid(CHUNK_SIZE);
    let lx = x.rem_euclid(CHUNK_SIZE);
    let ly = y.rem_euclid(CHUNK_SIZE);
    ((cx, cy), (lx, ly))
}

/// Whether a world cell is within the vertical bounds of the world.
#[inline]
pub fn in_bounds(_x: i32, y: i32) -> bool {
    y >= 0 && y < WORLD_HEIGHT
}

/// A sparse collection of chunks. Used directly by the client (chunks arrive
/// from the network) and wrapped by the server (chunks are generated on
/// demand).
#[derive(Default)]
pub struct World {
    chunks: HashMap<ChunkCoord, Chunk>,
    /// Flow distance of *spreading* water, per cell, from its nearest source.
    /// Only cells listed here ever spread; water absent from the map is inert —
    /// that covers naturally generated lakes and any water restored from a save,
    /// so neither floods the world on its own. A poured source is recorded at
    /// distance 0; horizontal flow increments the distance and stops at
    /// [`WATER_SPREAD_MAX`]; falling water resets to 0 (a fresh source). The
    /// client never populates this — it is driven entirely by the server.
    water_dist: HashMap<(i32, i32), u8>,
}

impl World {
    pub fn new() -> Self {
        World {
            chunks: HashMap::new(),
            water_dist: HashMap::new(),
        }
    }

    pub fn has_chunk(&self, coord: ChunkCoord) -> bool {
        self.chunks.contains_key(&coord)
    }

    pub fn insert_chunk(&mut self, coord: ChunkCoord, chunk: Chunk) {
        self.chunks.insert(coord, chunk);
    }

    pub fn get_chunk(&self, coord: ChunkCoord) -> Option<&Chunk> {
        self.chunks.get(&coord)
    }

    #[allow(dead_code)] // part of the world API surface; not yet used by callers
    pub fn get_chunk_mut(&mut self, coord: ChunkCoord) -> Option<&mut Chunk> {
        self.chunks.get_mut(&coord)
    }

    /// Read a block. Out-of-bounds or ungenerated cells read as air.
    pub fn get_block(&self, x: i32, y: i32) -> BlockId {
        if !in_bounds(x, y) {
            return AIR;
        }
        let (coord, (lx, ly)) = to_chunk(x, y);
        self.chunks
            .get(&coord)
            .map(|c| c.get(lx, ly))
            .unwrap_or(AIR)
    }

    /// Pour a water *source* at `(x, y)` — flow distance 0, so it spreads — if its
    /// chunk is loaded. Returns whether the water was placed. Used when a player
    /// empties a water bucket; naturally generated water never becomes a source.
    pub fn place_water_source(&mut self, x: i32, y: i32) -> bool {
        if self.set_block(x, y, WATER) {
            self.water_dist.insert((x, y), 0);
            true
        } else {
            false
        }
    }

    /// Flow spreading water one step outward. Each cell with a recorded flow
    /// distance fills the open cell below it (resetting the distance to 0, since
    /// falling water is a fresh source) and, while under [`WATER_SPREAD_MAX`], the
    /// open cells to either side (distance + 1). Only cells inside already-loaded
    /// chunks are filled, so spreading never generates fresh terrain. Volume is
    /// not conserved. Returns the freshly filled cells so the caller can mark them
    /// dirty and broadcast them.
    pub fn spread_water_once(&mut self) -> Vec<(i32, i32)> {
        // Snapshot the spreadable cells up front, so cells filled during this pass
        // don't cascade in one call and the borrow of the map is released before we
        // mutate it.
        let snapshot: Vec<((i32, i32), u8)> =
            self.water_dist.iter().map(|(&k, &v)| (k, v)).collect();

        let mut filled = Vec::new();
        for ((x, y), d) in snapshot {
            // Drop entries whose cell is no longer water (e.g. scooped up).
            if self.get_block(x, y) != WATER {
                self.water_dist.remove(&(x, y));
                continue;
            }
            // Down first (a fresh source), then the two sides if still in budget.
            let mut targets = vec![(x, y + 1, 0u8)];
            if d < WATER_SPREAD_MAX {
                targets.push((x - 1, y, d + 1));
                targets.push((x + 1, y, d + 1));
            }
            for (nx, ny, nd) in targets {
                if !in_bounds(nx, ny) {
                    continue;
                }
                // Only flow within an already-loaded chunk (never generate terrain).
                if !self.chunks.contains_key(&to_chunk(nx, ny).0) {
                    continue;
                }
                if self.get_block(nx, ny) == AIR && self.set_block(nx, ny, WATER) {
                    self.water_dist.insert((nx, ny), nd);
                    filled.push((nx, ny));
                }
            }
        }
        filled
    }

    /// Write a block if its chunk is present. Returns whether it was written.
    pub fn set_block(&mut self, x: i32, y: i32, b: BlockId) -> bool {
        if !in_bounds(x, y) {
            return false;
        }
        let (coord, (lx, ly)) = to_chunk(x, y);
        if let Some(chunk) = self.chunks.get_mut(&coord) {
            chunk.set(lx, ly, b);
            // Any non-water write clears a stale flow-distance record, so a cell
            // that stops being water (scooped, overwritten) becomes inert if it is
            // ever water again. Spreading sets WATER and records the distance itself.
            if b != WATER {
                self.water_dist.remove(&(x, y));
            }
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::STONE;

    #[test]
    fn water_spreads_down_and_sideways_but_never_up() {
        let mut world = World::new();
        world.insert_chunk((0, 0), Chunk::empty());
        world.place_water_source(5, 5);

        let filled = world.spread_water_once();
        // Flows into the cell below and to both sides.
        assert!(filled.contains(&(5, 6)));
        assert!(filled.contains(&(4, 5)));
        assert!(filled.contains(&(6, 5)));
        // Never climbs upward.
        assert!(!filled.contains(&(5, 4)));
        assert_eq!(world.get_block(5, 4), AIR);

        // A further pass keeps flowing outward from the cells just filled.
        assert!(!world.spread_water_once().is_empty());
    }

    #[test]
    fn naturally_placed_water_is_inert_and_does_not_spread() {
        let mut world = World::new();
        world.insert_chunk((0, 0), Chunk::empty());
        // Water written directly (as worldgen and save-loading do) carries no flow
        // distance, so it never spreads on its own.
        world.set_block(5, 5, WATER);
        assert!(world.spread_water_once().is_empty());
    }

    #[test]
    fn horizontal_spread_is_capped_at_max_distance() {
        let mut world = World::new();
        for cx in -1..=1 {
            world.insert_chunk((cx, 0), Chunk::empty());
        }
        // A solid floor across the loaded columns so water only spreads sideways.
        for x in (-CHUNK_SIZE)..(2 * CHUNK_SIZE) {
            world.set_block(x, 8, STONE);
        }
        world.place_water_source(0, 7); // sitting on the floor
        for _ in 0..40 {
            world.spread_water_once();
        }
        // Water reaches exactly WATER_SPREAD_MAX cells to either side, no further.
        let max = WATER_SPREAD_MAX as i32;
        assert_eq!(world.get_block(max, 7), WATER);
        assert_eq!(world.get_block(max + 1, 7), AIR);
        assert_eq!(world.get_block(-max, 7), WATER);
        assert_eq!(world.get_block(-(max + 1), 7), AIR);
    }

    #[test]
    fn water_does_not_flow_into_unloaded_chunks_or_through_solids() {
        let mut world = World::new();
        world.insert_chunk((0, 0), Chunk::empty());
        // Source at the chunk's right edge: its right neighbour is in an unloaded
        // chunk, and the cell below is solid stone.
        world.place_water_source(CHUNK_SIZE - 1, 5);
        world.set_block(CHUNK_SIZE - 1, 6, STONE);

        let filled = world.spread_water_once();
        // Only the open, loaded left neighbour fills.
        assert!(filled.contains(&(CHUNK_SIZE - 2, 5)));
        // The unloaded chunk to the right stays dry...
        assert!(!filled.iter().any(|&(x, _)| x >= CHUNK_SIZE));
        assert!(!world.has_chunk((1, 0)));
        // ...and the solid cell below is untouched.
        assert_eq!(world.get_block(CHUNK_SIZE - 1, 6), STONE);
    }
}
