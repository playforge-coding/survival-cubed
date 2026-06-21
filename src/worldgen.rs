//! Procedural terrain generation using `fastnoise-lite`.
//!
//! The world is split into [`Biome`]s by a low-frequency noise field: broad
//! flat **plains** (the common case), lush **forest** (flat like plains but
//! thick with trees) and rugged **mountains**. Each biome picks its own surface
//! roughness, block palette and tree density, so a single column's height and
//! blocks are decided entirely by the biome it falls in.

use fastnoise_lite::{FastNoiseLite, NoiseType};

use crate::block::{AIR, DIRT, GRASS, LEAVES, LOG, STONE};
use crate::world::{CHUNK_SIZE, Chunk, WORLD_HEIGHT, to_chunk};

/// Average surface row (cells from the top of the world).
const SURFACE_BASE: i32 = WORLD_HEIGHT / 2;
/// Number of dirt cells beneath the grass before stone begins (plains only).
const DIRT_DEPTH: i32 = 4;

/// Surface deviation amplitude (cells) for the gently rolling plains.
const PLAINS_AMP: f32 = 6.0;
/// Surface deviation amplitude (cells) for the rugged mountains.
const MOUNTAIN_AMP: f32 = 40.0;
/// Mountains sit this many cells higher (smaller row) than the plains baseline,
/// so a biome edge reads as terrain rising into a range.
const MOUNTAIN_LIFT: i32 = 14;

/// Biome noise above this threshold is mountains; below `FOREST_THRESHOLD` is
/// forest; the band between is plains. Biased so plains stay the common biome.
const MOUNTAIN_THRESHOLD: f32 = 0.30;
/// Biome noise below this threshold is forest (flat, tree-dense grassland).
const FOREST_THRESHOLD: f32 = -0.30;

/// Per-mille chance that any individual plains column roots a tree — sparse,
/// the odd lonely tree dotting the grassland.
const PLAINS_TREE_CHANCE: u32 = 30;
/// Per-mille chance for forest columns — abundant, a near-continuous canopy.
const FOREST_TREE_CHANCE: u32 = 340;
/// Trunk height range (cells of log above the ground) for a generated tree.
const TRUNK_MIN: i32 = 4;
const TRUNK_MAX: i32 = 6;
/// Canopy reaches this many cells out from the trunk top in each direction.
const CANOPY_RADIUS: i32 = 2;
/// Half-width (in biome-noise units) of the band around `MOUNTAIN_THRESHOLD`
/// over which terrain height is blended from plains to mountains. Wider means
/// longer, gentler foothills; `0.0` would restore hard cliffs at the boundary.
const BIOME_BLEND: f32 = 0.22;

/// A region of the world with its own terrain shape, block palette, and
/// creatures. Selected per column by a low-frequency noise field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Biome {
    /// Common, flat grassland: grass over dirt over stone, sparsely treed.
    Plains,
    /// Flat grassland like the plains but blanketed in trees.
    Forest,
    /// Rugged high ground built entirely of stone.
    Mountains,
}

pub struct WorldGen {
    seed: i32,
    height_noise: FastNoiseLite,
    biome_noise: FastNoiseLite,
}

impl WorldGen {
    pub fn new(seed: i32) -> Self {
        let mut height_noise = FastNoiseLite::with_seed(seed);
        height_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        height_noise.set_frequency(Some(0.012));
        // A separate, lower-frequency field so biomes span many columns. Offset
        // the seed so the biome map isn't correlated with the height map.
        let mut biome_noise = FastNoiseLite::with_seed(seed.wrapping_add(0x5EED));
        biome_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        biome_noise.set_frequency(Some(0.004));
        WorldGen {
            seed,
            height_noise,
            biome_noise,
        }
    }

    /// The seed this generator was built from. Persisted so a reloaded world
    /// reproduces the same terrain for chunks that were never modified.
    pub fn seed(&self) -> i32 {
        self.seed
    }

    /// How "mountainous" a column is, from `0.0` (full plains) to `1.0` (full
    /// mountains), ramped smoothly across the boundary so heights can be blended.
    /// Crosses `0.5` exactly at `MOUNTAIN_THRESHOLD`, keeping it consistent with
    /// [`biome_at`].
    fn mountain_weight(&self, world_x: i32) -> f32 {
        let n = self.biome_noise.get_noise_2d(world_x as f32, 0.0); // -1..1
        smoothstep(
            MOUNTAIN_THRESHOLD - BIOME_BLEND,
            MOUNTAIN_THRESHOLD + BIOME_BLEND,
            n,
        )
    }

    /// Which biome the given world column belongs to. A hard classification
    /// (used for the block palette and creature spawns); the terrain *height*
    /// still blends across the boundary via [`mountain_weight`].
    pub fn biome_at(&self, world_x: i32) -> Biome {
        let n = self.biome_noise.get_noise_2d(world_x as f32, 0.0); // -1..1
        if n > MOUNTAIN_THRESHOLD {
            Biome::Mountains
        } else if n < FOREST_THRESHOLD {
            Biome::Forest
        } else {
            Biome::Plains
        }
    }

    /// Surface row (the topmost solid cell) for a given world column. The two
    /// biomes' heights are interpolated by [`mountain_weight`] so the boundary
    /// rolls up into foothills instead of a sheer cliff.
    pub fn surface_height(&self, world_x: i32) -> i32 {
        let n = self.height_noise.get_noise_2d(world_x as f32, 0.0); // -1..1
        let plains_h = SURFACE_BASE as f32 + n * PLAINS_AMP;
        let mountain_h = (SURFACE_BASE - MOUNTAIN_LIFT) as f32 + n * MOUNTAIN_AMP;
        let w = self.mountain_weight(world_x);
        (plains_h + (mountain_h - plains_h) * w).round() as i32
    }

    /// The block to place at `world_y` in a column whose surface is at `surface`
    /// and that belongs to `biome`. `world_y < surface` is air (the caller skips
    /// those cells).
    fn block_at(biome: Biome, surface: i32, world_y: i32) -> crate::protocol::BlockId {
        match biome {
            // Mountains are bare stone from the surface down.
            Biome::Mountains => STONE,
            // Plains and forest share the classic grass / dirt band / stone
            // layering; they differ only in how many trees grow on top.
            Biome::Plains | Biome::Forest => {
                if world_y == surface {
                    GRASS
                } else if world_y <= surface + DIRT_DEPTH {
                    DIRT
                } else {
                    STONE
                }
            }
        }
    }

    /// Generate a chunk's worth of blocks.
    pub fn generate_chunk(&self, cx: i32, cy: i32) -> Chunk {
        let mut chunk = Chunk::empty();
        let base_x = cx * CHUNK_SIZE;
        let base_y = cy * CHUNK_SIZE;
        for lx in 0..CHUNK_SIZE {
            let world_x = base_x + lx;
            let biome = self.biome_at(world_x);
            let surface = self.surface_height(world_x);
            for ly in 0..CHUNK_SIZE {
                let world_y = base_y + ly;
                if world_y < surface {
                    continue; // air
                }
                chunk.set(lx, ly, Self::block_at(biome, surface, world_y));
            }
        }
        // Trees grow upward from the surface and can lean their canopies across
        // chunk borders, so scan a margin of columns either side and let
        // [`place_tree`] clip whatever falls outside this chunk.
        for world_x in (base_x - CANOPY_RADIUS)..(base_x + CHUNK_SIZE + CANOPY_RADIUS) {
            if self.tree_root_at(world_x) {
                self.place_tree(&mut chunk, base_x, base_y, world_x);
            }
        }
        chunk
    }

    /// Deterministic pseudo-random value for a world column, mixed with `salt`
    /// to draw several independent decisions (tree presence, trunk height) from
    /// the same column. Seeded so a column always decides the same way.
    fn col_hash(&self, world_x: i32, salt: u32) -> u32 {
        let mut h = (self.seed as u32)
            .wrapping_mul(374_761_393)
            .wrapping_add((world_x as u32).wrapping_mul(668_265_263));
        h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
        h = h.wrapping_add(salt.wrapping_mul(0x9E37_79B9));
        h ^ (h >> 16)
    }

    /// Whether a column rolls a tree, by its biome's density. Independent of
    /// neighbours; spacing is enforced by [`tree_root_at`].
    fn column_rolls_tree(&self, world_x: i32) -> bool {
        let chance = match self.biome_at(world_x) {
            Biome::Forest => FOREST_TREE_CHANCE,
            Biome::Plains => PLAINS_TREE_CHANCE,
            Biome::Mountains => 0,
        };
        self.col_hash(world_x, 0) % 1000 < chance
    }

    /// Whether a tree's trunk is rooted at this column. A rolled column is
    /// suppressed if its left neighbour also rolled, so trunks never stand
    /// directly adjacent even in dense forest (their canopies still merge).
    fn tree_root_at(&self, world_x: i32) -> bool {
        self.column_rolls_tree(world_x) && !self.column_rolls_tree(world_x - 1)
    }

    /// Write a tree rooted at `root_x` into `chunk`, clipping to the chunk's
    /// cell bounds. Only ever fills air, so trees never gouge into terrain or
    /// overwrite a neighbouring trunk.
    fn place_tree(&self, chunk: &mut Chunk, base_x: i32, base_y: i32, root_x: i32) {
        let surface = self.surface_height(root_x);
        let span = (TRUNK_MAX - TRUNK_MIN + 1) as u32;
        let trunk_h = TRUNK_MIN + (self.col_hash(root_x, 1) % span) as i32;
        let top = surface - trunk_h; // topmost trunk cell (smaller y is higher)

        // Trunk first: a column of logs from just above the ground to `top`.
        for world_y in top..surface {
            put_block(chunk, base_x, base_y, root_x, world_y, LOG);
        }
        // Then a rounded canopy of leaves centred on the trunk top, filling only
        // the air around the logs.
        for oy in -CANOPY_RADIUS..=CANOPY_RADIUS {
            for ox in -CANOPY_RADIUS..=CANOPY_RADIUS {
                if ox * ox + oy * oy > CANOPY_RADIUS * CANOPY_RADIUS + 1 {
                    continue; // trim the corners for a rounded crown
                }
                put_block(chunk, base_x, base_y, root_x + ox, top + oy, LEAVES);
            }
        }
    }
}

/// Set a world cell within a chunk, ignoring writes that fall outside the chunk
/// or onto a cell that is not air. Used to clip trees to chunk bounds without
/// disturbing terrain or other trees.
fn put_block(
    chunk: &mut Chunk,
    base_x: i32,
    base_y: i32,
    world_x: i32,
    world_y: i32,
    b: crate::protocol::BlockId,
) {
    let lx = world_x - base_x;
    let ly = world_y - base_y;
    if !(0..CHUNK_SIZE).contains(&lx) || !(0..CHUNK_SIZE).contains(&ly) {
        return;
    }
    if chunk.get(lx, ly) == AIR {
        chunk.set(lx, ly, b);
    }
}

/// Hermite smoothstep: `0.0` below `edge0`, `1.0` above `edge1`, with a smooth
/// ease in between. Used to blend biome heights without a hard seam.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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
