//! Chunked tile world shared by client and server.
//!
//! The world is infinite horizontally and fixed height vertically. Positive
//! `y` points *down* (row 0 is the sky ceiling), matching screen space. Blocks
//! are addressed by integer cell coordinates; chunks group `CHUNK_SIZE` cells
//! per side.

use std::collections::HashMap;

use crate::block::AIR;
use crate::protocol::BlockId;

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
}

impl World {
    pub fn new() -> Self {
        World {
            chunks: HashMap::new(),
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

    /// Write a block if its chunk is present. Returns whether it was written.
    pub fn set_block(&mut self, x: i32, y: i32, b: BlockId) -> bool {
        if !in_bounds(x, y) {
            return false;
        }
        let (coord, (lx, ly)) = to_chunk(x, y);
        if let Some(chunk) = self.chunks.get_mut(&coord) {
            chunk.set(lx, ly, b);
            true
        } else {
            false
        }
    }
}
