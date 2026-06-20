//! Procedural terrain generation using `fastnoise-lite`.
//!
//! For now this produces a simple side-on landscape: a noisy surface height per
//! column, grass on top, a band of dirt, then stone all the way down.

use fastnoise_lite::{FastNoiseLite, NoiseType};

use crate::block::{DIRT, GRASS, STONE};
use crate::world::{CHUNK_SIZE, Chunk, WORLD_HEIGHT, to_chunk};

/// Average surface row (cells from the top of the world).
const SURFACE_BASE: i32 = WORLD_HEIGHT / 2;
/// Maximum surface deviation from `SURFACE_BASE`, in cells.
const SURFACE_AMP: f32 = 28.0;
/// Number of dirt cells beneath the grass before stone begins.
const DIRT_DEPTH: i32 = 4;

pub struct WorldGen {
    height_noise: FastNoiseLite,
}

impl WorldGen {
    pub fn new(seed: i32) -> Self {
        let mut height_noise = FastNoiseLite::with_seed(seed);
        height_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        height_noise.set_frequency(Some(0.012));
        WorldGen { height_noise }
    }

    /// Surface row (the grass cell) for a given world column.
    pub fn surface_height(&self, world_x: i32) -> i32 {
        let n = self.height_noise.get_noise_2d(world_x as f32, 0.0); // -1..1
        SURFACE_BASE + (n * SURFACE_AMP) as i32
    }

    /// Generate a chunk's worth of blocks.
    pub fn generate_chunk(&self, cx: i32, cy: i32) -> Chunk {
        let mut chunk = Chunk::empty();
        let base_x = cx * CHUNK_SIZE;
        let base_y = cy * CHUNK_SIZE;
        for lx in 0..CHUNK_SIZE {
            let world_x = base_x + lx;
            let surface = self.surface_height(world_x);
            for ly in 0..CHUNK_SIZE {
                let world_y = base_y + ly;
                let block = if world_y < surface {
                    continue; // air
                } else if world_y == surface {
                    GRASS
                } else if world_y <= surface + DIRT_DEPTH {
                    DIRT
                } else {
                    STONE
                };
                chunk.set(lx, ly, block);
            }
        }
        chunk
    }
}

/// Convenience: which chunk a spawn point at `world_x` should sit above, with
/// the player's feet just over the surface. Returns `(spawn_x_px, spawn_y_px)`.
pub fn spawn_point(generator: &WorldGen, world_x: i32) -> (f32, f32) {
    let surface = generator.surface_height(world_x);
    let _ = to_chunk(world_x, surface); // ensures coordinate helpers stay in use
    let x_px = world_x as f32 * crate::world::TILE_SIZE;
    // Place the player two cells above the surface so they drop onto the ground.
    let y_px = (surface - 3) as f32 * crate::world::TILE_SIZE;
    (x_px, y_px)
}
