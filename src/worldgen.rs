//! Procedural terrain generation using `fastnoise-lite`.
//!
//! The world is split into [`Biome`]s by a low-frequency noise field: broad
//! flat **plains** (the common case), lush **forest** (flat like plains but
//! thick with trees) and rugged **mountains**. Each biome picks its own surface
//! roughness, block palette and tree density, so a single column's height and
//! blocks are decided entirely by the biome it falls in.

use fastnoise_lite::{FastNoiseLite, NoiseType};

use crate::block::{AIR, COAL_ORE, DIRT, GRASS, IRON_ORE, LEAVES, LOG, STONE};
use crate::protocol::BlockId;
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

/// Iron ore only replaces stone at least this many cells below the surface, so
/// it never breaks the surface or sits in the dirt band.
const ORE_MIN_DEPTH: i32 = 6;
/// Ore-noise value above which a deep stone cell becomes iron ore. Higher means
/// rarer; underground plains/forest stone uses this stricter threshold.
const ORE_THRESHOLD: f32 = 0.55;
/// Lower (more generous) threshold inside mountains, which are iron-rich.
const MOUNTAIN_ORE_THRESHOLD: f32 = 0.40;

/// Coal ore sits shallower than iron, breaking into the dirt/stone boundary, so it
/// is the first fuel a fresh player can dig up.
const COAL_ORE_MIN_DEPTH: i32 = 3;
/// Coal-noise value above which a stone cell becomes coal ore. Lower (more
/// generous) than [`ORE_THRESHOLD`], making coal the more common ore.
const COAL_ORE_THRESHOLD: f32 = 0.45;

/// Half-width (in noise units) of the band around a cave-noise zero-contour that
/// gets carved into a winding tunnel. Larger means wider tunnels (paired with the
/// field's low frequency, this maps to a passage several cells across — wide
/// enough for the two-cell-tall player to walk through).
const TUNNEL_WIDTH: f32 = 0.06;
/// Cave-noise value above which a deep cell is hollowed into an open cavern.
const CAVERN_THRESHOLD: f32 = 0.55;
/// Caverns are only carved at least this many cells below the surface, keeping
/// the large open rooms deep underground while tunnels still reach daylight.
const CAVERN_MIN_DEPTH: i32 = 30;
/// The bottom-most rows of the world are never carved, leaving a solid floor so
/// caves and caverns can't open into the empty void beneath the world.
const BEDROCK_FLOOR: i32 = 4;

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
    ore_noise: FastNoiseLite,
    /// A second vein field, on its own seed, driving coal-ore placement
    /// independently of the iron veins.
    coal_noise: FastNoiseLite,
    /// Field whose zero-contour is carved into winding tunnels (see
    /// [`is_cave`](WorldGen::is_cave)).
    cave_noise: FastNoiseLite,
    /// Low-frequency field carving large open caverns deep underground.
    cavern_noise: FastNoiseLite,
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
        // A higher-frequency field driving compact ore veins, on its own seed so
        // ore placement is independent of terrain and biome shape.
        let mut ore_noise = FastNoiseLite::with_seed(seed.wrapping_add(0x0A11));
        ore_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        ore_noise.set_frequency(Some(0.09));
        // Coal veins: a sibling of the iron field on its own seed, so coal and
        // iron deposits don't track one another.
        let mut coal_noise = FastNoiseLite::with_seed(seed.wrapping_add(0xC0A1));
        coal_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        coal_noise.set_frequency(Some(0.09));
        // A low-frequency field for winding tunnels, carved as a band around its
        // zero contour. Low frequency keeps the gradient gentle, so the band maps
        // to several connected cells (a real passage, not a dotted line) and the
        // tunnels are long, sparse, sweeping curves rather than constant noise.
        let mut cave_noise = FastNoiseLite::with_seed(seed.wrapping_add(0xCA7E));
        cave_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        cave_noise.set_frequency(Some(0.014));
        // A low-frequency field whose peaks become big open caverns deep down.
        let mut cavern_noise = FastNoiseLite::with_seed(seed.wrapping_add(0xCA77));
        cavern_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        cavern_noise.set_frequency(Some(0.022));
        WorldGen {
            seed,
            height_noise,
            biome_noise,
            ore_noise,
            coal_noise,
            cave_noise,
            cavern_noise,
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

    /// Which ore (if any) a stone cell at `(world_x, world_y)` should become. Ore
    /// forms compact veins (driven by [`ore_noise`](WorldGen::ore_noise) and
    /// [`coal_noise`](WorldGen::coal_noise)) underground; iron sits deep and is
    /// richer inside mountains, while coal runs shallower and more abundantly
    /// everywhere. Iron wins where the two veins overlap. Only ever called for
    /// cells that would otherwise be stone.
    fn ore_at(&self, world_x: i32, world_y: i32, surface: i32, biome: Biome) -> BlockId {
        if world_y - surface >= ORE_MIN_DEPTH {
            let v = self
                .ore_noise
                .get_noise_3d(world_x as f32, world_y as f32, 0.0); // -1..1
            let threshold = if biome == Biome::Mountains {
                MOUNTAIN_ORE_THRESHOLD
            } else {
                ORE_THRESHOLD
            };
            if v > threshold {
                return IRON_ORE;
            }
        }
        if world_y - surface >= COAL_ORE_MIN_DEPTH {
            let v = self
                .coal_noise
                .get_noise_3d(world_x as f32, world_y as f32, 0.0); // -1..1
            if v > COAL_ORE_THRESHOLD {
                return COAL_ORE;
            }
        }
        STONE
    }

    /// Whether the ground cell at `(world_x, world_y)` should be hollowed out as
    /// a cave. Two kinds are carved: thin **winding tunnels** along the zero
    /// contours of two noise fields (which branch where they cross and break the
    /// surface where a contour reaches it), and large **caverns** deep down where
    /// a low-frequency field peaks. Only meaningful for cells at or below the
    /// surface (the caller skips the open air above).
    fn is_cave(&self, world_x: i32, world_y: i32, surface: i32) -> bool {
        // Keep a solid floor at the very bottom of the world.
        if world_y >= WORLD_HEIGHT - BEDROCK_FLOOR {
            return false;
        }
        let (x, y) = (world_x as f32, world_y as f32);
        // Winding tunnels: carve a band either side of the field's zero contour.
        // Reaching up to the surface, these are the caves that open to daylight.
        if self.cave_noise.get_noise_2d(x, y).abs() < TUNNEL_WIDTH {
            return true;
        }
        // Deep caverns: big open rooms, only well below the surface.
        if world_y - surface >= CAVERN_MIN_DEPTH
            && self.cavern_noise.get_noise_2d(x, y) > CAVERN_THRESHOLD
        {
            return true;
        }
        false
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
                    continue; // air above the ground
                }
                // Caves are carved out of solid ground: leave the cell as air.
                if self.is_cave(world_x, world_y, surface) {
                    continue;
                }
                let mut block = Self::block_at(biome, surface, world_y);
                // Scatter iron ore through deep stone (and mountain interiors).
                if block == STONE {
                    block = self.ore_at(world_x, world_y, surface, biome);
                }
                chunk.set(lx, ly, block);
            }
        }
        // Trees grow upward from the surface and can lean their canopies across
        // chunk borders, so scan a margin of columns either side and let
        // [`place_tree`] clip whatever falls outside this chunk. Skip columns
        // whose surface a cave has opened, so trees don't float over cave mouths.
        for world_x in (base_x - CANOPY_RADIUS)..(base_x + CHUNK_SIZE + CANOPY_RADIUS) {
            let surface = self.surface_height(world_x);
            if self.tree_root_at(world_x) && !self.is_cave(world_x, surface, surface) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{AIR, IRON_ORE};

    /// Caves hollow out a meaningful amount of underground rock, but nowhere near
    /// all of it — the world stays mostly solid — and iron ore still survives the
    /// carving.
    #[test]
    fn caves_carve_underground_without_voiding_the_world() {
        let worldgen = WorldGen::new(0xC0FFEE);
        let (mut underground, mut air, mut ore) = (0u32, 0u32, 0u32);
        // A wide, deep swath of the world.
        for cx in -4..4 {
            for cy in 0..16 {
                let chunk = worldgen.generate_chunk(cx, cy);
                for lx in 0..CHUNK_SIZE {
                    let world_x = cx * CHUNK_SIZE + lx;
                    let surface = worldgen.surface_height(world_x);
                    for ly in 0..CHUNK_SIZE {
                        let world_y = cy * CHUNK_SIZE + ly;
                        if world_y <= surface + 5 {
                            continue; // ignore the open air and surface band
                        }
                        underground += 1;
                        match chunk.get(lx, ly) {
                            AIR => air += 1,
                            IRON_ORE => ore += 1,
                            _ => {}
                        }
                    }
                }
            }
        }
        assert!(underground > 0);
        assert!(air > 0, "expected caves to carve some underground air");
        assert!(
            air < underground / 2,
            "caves hollowed out too much: {air}/{underground} cells are air"
        );
        assert!(ore > 0, "iron ore should still generate alongside caves");
    }

    /// Dev helper: print an ASCII slice of the world so cave shapes can be eyeballed.
    /// Run with `cargo test print_slice -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn print_slice() {
        let worldgen = WorldGen::new(0xC0FFEE);
        let x0 = 0;
        let width = 160;
        let y0 = SURFACE_BASE - 8;
        // Render all the way to the world bottom so the bedrock floor is visible.
        for y in y0..WORLD_HEIGHT {
            let mut line = String::new();
            for x in x0..(x0 + width) {
                let surface = worldgen.surface_height(x);
                let ch = if y < surface {
                    '.' // open sky
                } else if worldgen.is_cave(x, y, surface) {
                    ' ' // carved cave
                } else {
                    '#' // solid
                };
                line.push(ch);
            }
            eprintln!("{line}");
        }
    }

    /// The world's bottom rows are always solid, so no cave or cavern opens into
    /// the void beneath the world.
    #[test]
    fn world_has_a_solid_bedrock_floor() {
        let worldgen = WorldGen::new(0xC0FFEE);
        let bottom_cy = WORLD_HEIGHT / CHUNK_SIZE - 1;
        for cx in -8..8 {
            let chunk = worldgen.generate_chunk(cx, bottom_cy);
            for lx in 0..CHUNK_SIZE {
                for ly in (CHUNK_SIZE - BEDROCK_FLOOR)..CHUNK_SIZE {
                    assert_ne!(
                        chunk.get(lx, ly),
                        AIR,
                        "bottom row carved open at chunk {cx}, cell ({lx}, {ly})"
                    );
                }
            }
        }
    }

    /// At least some winding tunnels break the surface, opening caves to daylight.
    #[test]
    fn some_caves_reach_the_surface() {
        let worldgen = WorldGen::new(0xC0FFEE);
        let openings = (-2000..2000)
            .filter(|&x| {
                let s = worldgen.surface_height(x);
                worldgen.is_cave(x, s, s)
            })
            .count();
        assert!(openings > 0, "expected some caves to open at the surface");
    }
}
