//! Procedural terrain generation using `fastnoise-lite`.
//!
//! The world is split into [`Biome`]s by low-frequency noise fields: broad flat
//! **plains** (the common case), lush **forest** (flat like plains but thick with
//! trees), rugged **mountains**, sandy **desert**, and deep **ocean** (a column
//! sunk below sea level and flooded with water over a sandy seabed). Each biome
//! picks its own surface roughness, block palette and tree density, so a single
//! column's height and blocks are decided entirely by the biome it falls in.

use fastnoise_lite::{FastNoiseLite, NoiseType};

use crate::block::{
    AIR, ASH, CHARRED_ROCK, COAL_ORE, DIRT, FIRE, GOLD_ORE, GRASS, IRON_ORE, LEAVES, LOG, SAND,
    STONE, TUNGSTEN_ORE, WATER,
};
use crate::entity::EntityKind;
use crate::protocol::BlockId;
use crate::structure::Structure;
use crate::world::{CHUNK_SIZE, Chunk, Dimension, TILE_SIZE, WORLD_HEIGHT, to_chunk};

/// A structure embedded in the binary and scattered across the overworld surface
/// during generation. The bytes are the exact format creators save (see
/// [`crate::structure`]), so a hand-built ruin can be dropped in here.
const EMBEDDED_RUIN: &[u8] = include_bytes!("../assets/structures/ruin.scst");

/// Per-mille chance a lowland column roots an embedded ruin — rare, so they read
/// as occasional landmarks rather than clutter.
const STRUCTURE_CHANCE: u32 = 4;

/// Average surface row (cells from the top of the world).
const SURFACE_BASE: i32 = WORLD_HEIGHT / 2;
/// Row of the water surface. Any column whose ground dips below this row is
/// flooded with water from here down to the sea floor, forming ponds, lakes, and
/// the occasional deep mountain-valley pool. Sits a touch below [`SURFACE_BASE`]
/// so only genuine dips hold water and the rolling plains stay mostly dry.
const SEA_LEVEL: i32 = SURFACE_BASE + 2;
/// Number of dirt cells beneath the grass before stone begins (plains only).
const DIRT_DEPTH: i32 = 4;
/// Number of sand cells from the surface down before stone begins (desert only).
const SAND_DEPTH: i32 = 4;

/// Surface deviation amplitude (cells) for the gently rolling plains.
const PLAINS_AMP: f32 = 6.0;
/// Surface deviation amplitude (cells) for the rugged mountains.
const MOUNTAIN_AMP: f32 = 40.0;
/// Mountains sit this many cells higher (smaller row) than the plains baseline,
/// so a biome edge reads as terrain rising into a range.
const MOUNTAIN_LIFT: i32 = 14;

/// Biome noise above this threshold is mountains; below `FOREST_THRESHOLD` is
/// forest; the wide band between is plains. The field is nearly uniform over its
/// `-1..1` range, so this sits far out near the top: only the noise's rare peaks
/// rise into mountains, leaving them an occasional range cresting out of the
/// grassland rather than a common biome. The plains they used to interrupt merge
/// into broad sweeps as a result.
const MOUNTAIN_THRESHOLD: f32 = 0.85;
/// Biome noise below this threshold is forest (flat, tree-dense grassland). The
/// lower bookend of the wide plains band.
const FOREST_THRESHOLD: f32 = -0.30;
/// Value of the dedicated desert field (see [`WorldGen::desert_noise`]) above
/// which a non-mountain column becomes desert: flat, sandy, treeless terrain at
/// plains height. The field is low-frequency and independent of the main biome
/// axis, so deserts form broad, contiguous swaths many chunks across rather than
/// thin strips squeezed between forest and plains. Like the mountain cutoff this
/// sits far out near the top of the (near-uniform) field, so only its rare peaks
/// turn to sand — deserts are an uncommon feature, not a staple. Lower means more
/// (and larger) deserts.
const DESERT_THRESHOLD: f32 = 0.85;
/// Value of the dedicated ocean field (see [`WorldGen::ocean_noise`]) above which
/// a column becomes ocean: its surface sinks far below sea level so the column
/// floods into a deep, open body of water over a sandy floor. The field is its own
/// low-frequency axis, so oceans form broad, contiguous seas many chunks across
/// rather than thin channels. Lower means more (and larger) oceans.
const OCEAN_THRESHOLD: f32 = 0.38;
/// Half-width (in ocean-noise units) of the band over which a column's surface is
/// blended from land height down to the ocean floor, so the shore slopes into the
/// depths instead of dropping off a cliff. The midpoint flips [`WorldGen::biome_at`].
const OCEAN_BLEND: f32 = 0.12;
/// Cells the ocean floor sits below [`SEA_LEVEL`]: the depth of open water over the
/// seabed at the heart of an ocean (the shore is shallower as the floor blends up).
const OCEAN_DEPTH: i32 = 20;
/// Surface deviation amplitude (cells) of the gently undulating sea floor.
const OCEAN_FLOOR_AMP: f32 = 3.0;

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
/// Thickness (cells) of the surface band in which a tunnel may only be carved
/// where the ground is steep (see [`STEEP_THRESHOLD`]). Over flat terrain this
/// band stays solid, so caves never punch a hole straight down into level
/// ground; on a hill or mountain face it is carved through, letting the tunnel
/// open in the slope. Applies to both dimensions (measured from the overworld
/// surface or the underworld ceiling).
const SURFACE_CRUST: i32 = 6;
/// Horizontal half-span (cells) over which a column's surface steepness is
/// measured when deciding whether a cave may break the surface there.
const SLOPE_SPAN: i32 = 2;
/// A column counts as a steep **face** — a hill or mountain side a cave is
/// allowed to open through — when its surface rises or falls by at least this
/// many cells across the [`SLOPE_SPAN`] to either side. Gentle plains never
/// reach it, so their caves stay sealed beneath a solid crust; only genuine
/// slopes get a sideways cave mouth. Lower means mouths on shallower hills.
const STEEP_THRESHOLD: i32 = 5;

// --- Underworld ----------------------------------------------------------

/// Average row of the underworld's solid charred-rock **ceiling**: the roof fills
/// every column from the very top of the world (row 0) down to here, and the
/// first open cavern cell hangs just beneath it. Its gently undulating underside
/// is the ceiling a player sees overhead — the underworld is a closed vault, not
/// open sky. A player climbing back up must dig through this roof to leave.
const UNDERWORLD_CEILING_BASE: i32 = 10;
/// Surface deviation amplitude (cells) of the gently uneven charred-rock ceiling.
const UNDERWORLD_CEILING_AMP: f32 = 4.0;
/// Height (cells) of the open cavern between the ceiling and the charred-rock
/// floor below. Tall, so the underworld reads as a vast vaulted hall a player can
/// build and climb up through — far more headroom than a low crawlspace.
const UNDERWORLD_CAVERN_HEIGHT: i32 = 80;
/// Average **floor** row of the underworld: the topmost solid charred rock a
/// player walks on, a full cavern's height below the ceiling. The player warps in
/// onto this floor; everything from here down to bedrock is the diggable charred
/// depths (tungsten, ash valleys, caves).
const UNDERWORLD_SURFACE_BASE: i32 = UNDERWORLD_CEILING_BASE + UNDERWORLD_CAVERN_HEIGHT;
/// Surface deviation amplitude (cells) of the gently uneven charred-rock floor.
const UNDERWORLD_SURFACE_AMP: f32 = 4.0;
/// Per-mille chance that an exposed charred-rock floor cell (open above, solid
/// below) sprouts a tongue of natural fire. Sparse, so the underworld glows with
/// scattered flames rather than being a wall of fire.
const UNDERWORLD_FIRE_CHANCE: u32 = 70;
/// Tungsten ore only replaces charred rock at least this many cells below the
/// underworld ceiling, keeping it out of the exposed surface the player lands on.
const TUNGSTEN_ORE_MIN_DEPTH: i32 = 6;
/// Tungsten-noise value above which a deep charred-rock cell becomes tungsten ore.
/// High, so the strongest material stays a rare reward for delving the underworld.
const TUNGSTEN_ORE_THRESHOLD: f32 = 0.6;
/// Gold ore only replaces charred rock at least this many cells below the
/// underworld ceiling — shallower than tungsten, but still off the landing surface.
const GOLD_ORE_MIN_DEPTH: i32 = 4;
/// Gold-noise value above which a charred-rock cell becomes gold ore. Higher than
/// the tungsten threshold, so gold veins are scarcer still.
const GOLD_ORE_THRESHOLD: f32 = 0.72;

/// Underworld biome noise below this threshold is an ash valley: its surface band
/// is loose ash over the usual charred rock. Elsewhere the underworld is its
/// default charred expanse. Reuses the overworld [`WorldGen::biome_noise`] field,
/// independent across dimensions.
const ASH_VALLEY_THRESHOLD: f32 = -0.15;
/// Number of ash cells from the underworld ceiling down before charred rock
/// begins (ash valleys only).
const ASH_DEPTH: i32 = 4;

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
    /// Flat, treeless dunes: sand over stone.
    Desert,
    /// A deep, open sea: the column floods with water from sea level down to a
    /// sandy floor far below. Treeless and (for now) lifeless — cross it by boat.
    Ocean,
}

/// A region of the *underworld*, the charred layer beneath the overworld.
/// Selected per column like an overworld [`Biome`], but with its own palette.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnderworldBiome {
    /// The default expanse: charred rock from the ceiling down.
    Charred,
    /// An ashen basin: a band of loose ash blankets the charred rock surface.
    AshValley,
}

pub struct WorldGen {
    seed: i32,
    height_noise: FastNoiseLite,
    biome_noise: FastNoiseLite,
    /// A dedicated low-frequency field carving broad deserts out of the lower,
    /// non-mountainous ground, independent of the plains/forest split.
    desert_noise: FastNoiseLite,
    /// A dedicated low-frequency field, on its own seed, carving broad oceans:
    /// where it peaks the surface is sunk far below sea level into a deep sea.
    ocean_noise: FastNoiseLite,
    ore_noise: FastNoiseLite,
    /// A second vein field, on its own seed, driving coal-ore placement
    /// independently of the iron veins.
    coal_noise: FastNoiseLite,
    /// Field whose zero-contour is carved into winding tunnels (see
    /// [`is_cave`](WorldGen::is_cave)).
    cave_noise: FastNoiseLite,
    /// Low-frequency field carving large open caverns deep underground.
    cavern_noise: FastNoiseLite,
    /// Field shaping the underworld's charred-rock ceiling height.
    uw_height_noise: FastNoiseLite,
    /// Field whose zero-contour is carved into the underworld's winding tunnels,
    /// the charred-depths counterpart of [`cave_noise`](WorldGen::cave_noise).
    uw_cave_noise: FastNoiseLite,
    /// Low-frequency field carving large open caverns deep in the charred
    /// depths, the underworld counterpart of [`cavern_noise`](WorldGen::cavern_noise).
    uw_cavern_noise: FastNoiseLite,
    /// Vein field driving tungsten-ore placement in the underworld's charred rock,
    /// on its own seed so the strongest ore isn't correlated with the caves.
    tungsten_noise: FastNoiseLite,
    /// Vein field driving gold-ore placement in the underworld's charred rock, on
    /// its own seed so gold veins are independent of tungsten and the caves.
    gold_noise: FastNoiseLite,
    /// The embedded structure scattered across the surface, parsed once. `None`
    /// if the embedded bytes fail to parse (then no structures are placed).
    structure: Option<Structure>,
}

impl WorldGen {
    pub fn new(seed: i32) -> Self {
        let mut height_noise = FastNoiseLite::with_seed(seed);
        height_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        height_noise.set_frequency(Some(0.012));
        // A separate, low-frequency field so biomes span many columns. Offset the
        // seed so the biome map isn't correlated with the height map. The frequency
        // is kept low so each biome — above all the dominant plains — stretches over
        // broad, sweeping regions rather than small, frequently-changing patches.
        let mut biome_noise = FastNoiseLite::with_seed(seed.wrapping_add(0x5EED));
        biome_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        biome_noise.set_frequency(Some(0.003));
        // A second low-frequency field, on its own seed, laid over the biome map
        // to carve broad deserts. Its frequency sets desert size — lower spreads
        // them wider; this is tuned for swaths several chunks across.
        let mut desert_noise = FastNoiseLite::with_seed(seed.wrapping_add(0xDE5E));
        desert_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        desert_noise.set_frequency(Some(0.0045));
        // The ocean field: another low-frequency axis on its own seed, lower in
        // frequency than the desert field so seas spread wide and contiguous.
        let mut ocean_noise = FastNoiseLite::with_seed(seed.wrapping_add(0x0CEA));
        ocean_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        ocean_noise.set_frequency(Some(0.0035));
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
        // Underworld fields, on their own seeds so the charred layer's shape is
        // independent of the surface above it. The ceiling rolls slowly; the cave
        // and cavern fields mirror the overworld's frequencies, so the charred
        // depths are riddled with the same kind of winding tunnels and deep open
        // caverns found beneath the surface above.
        let mut uw_height_noise = FastNoiseLite::with_seed(seed.wrapping_add(0x4DEE));
        uw_height_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        uw_height_noise.set_frequency(Some(0.02));
        let mut uw_cave_noise = FastNoiseLite::with_seed(seed.wrapping_add(0x4CA7));
        uw_cave_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        uw_cave_noise.set_frequency(Some(0.014));
        let mut uw_cavern_noise = FastNoiseLite::with_seed(seed.wrapping_add(0x4C77));
        uw_cavern_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        uw_cavern_noise.set_frequency(Some(0.022));
        // Tungsten veins through the charred rock: a compact, high-frequency field
        // like the overworld ore veins, on its own seed.
        let mut tungsten_noise = FastNoiseLite::with_seed(seed.wrapping_add(0x7079));
        tungsten_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        tungsten_noise.set_frequency(Some(0.09));
        // Gold veins: a similar compact field on its own seed, so gold scatters
        // through the charred rock independently of tungsten.
        let mut gold_noise = FastNoiseLite::with_seed(seed.wrapping_add(0x601D));
        gold_noise.set_noise_type(Some(NoiseType::OpenSimplex2));
        gold_noise.set_frequency(Some(0.10));
        WorldGen {
            seed,
            height_noise,
            biome_noise,
            desert_noise,
            ocean_noise,
            ore_noise,
            coal_noise,
            cave_noise,
            cavern_noise,
            uw_height_noise,
            uw_cave_noise,
            uw_cavern_noise,
            tungsten_noise,
            gold_noise,
            structure: Structure::from_bytes(EMBEDDED_RUIN)
                .ok()
                .filter(|s| !s.is_empty()),
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

    /// How strongly a column is ocean, from `0.0` (dry land) to `1.0` (deep open
    /// sea), ramped smoothly across the shore so the seabed can be blended down.
    /// Crosses `0.5` exactly at [`OCEAN_THRESHOLD`], keeping it consistent with
    /// [`biome_at`].
    fn ocean_weight(&self, world_x: i32) -> f32 {
        let n = self.ocean_noise.get_noise_2d(world_x as f32, 0.0); // -1..1
        smoothstep(
            OCEAN_THRESHOLD - OCEAN_BLEND,
            OCEAN_THRESHOLD + OCEAN_BLEND,
            n,
        )
    }

    /// Which biome the given world column belongs to. A hard classification
    /// (used for the block palette and creature spawns); the terrain *height*
    /// still blends across the boundary via [`mountain_weight`] and
    /// [`ocean_weight`](Self::ocean_weight).
    pub fn biome_at(&self, world_x: i32) -> Biome {
        // A deep ocean overrides the land biomes: its own field has already sunk
        // the surface below sea level, so the column is open water over a seabed.
        if self.ocean_weight(world_x) >= 0.5 {
            return Biome::Ocean;
        }
        let n = self.biome_noise.get_noise_2d(world_x as f32, 0.0); // -1..1
        // Mountains win outright — no sand peaks.
        if n > MOUNTAIN_THRESHOLD {
            return Biome::Mountains;
        }
        // A dedicated low-frequency field carves broad deserts out of the rest of
        // the lower ground, independent of the plains/forest split.
        if self.desert_noise.get_noise_2d(world_x as f32, 0.0) > DESERT_THRESHOLD {
            return Biome::Desert;
        }
        if n < FOREST_THRESHOLD {
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
        let land_h = plains_h + (mountain_h - plains_h) * self.mountain_weight(world_x);
        // Oceans pull the surface deep below sea level (blended in along the shore),
        // so the column floods into open water over a sandy seabed.
        let ocean_h = (SEA_LEVEL + OCEAN_DEPTH) as f32 + n * OCEAN_FLOOR_AMP;
        (land_h + (ocean_h - land_h) * self.ocean_weight(world_x)).round() as i32
    }

    /// A column near the origin fit to spawn a player on: dry land (surface above
    /// sea level, so not in a pond or ocean) that no cave has broken open. Scans
    /// outward from `x = 0` so a world whose origin happens to fall in the sea
    /// still drops the player onto a nearby shore rather than into deep water.
    pub fn land_spawn_x(&self) -> i32 {
        for d in 0..10_000 {
            for x in [d, -d] {
                let s = self.surface_height(x);
                if self.biome_at(x) != Biome::Ocean && s < SEA_LEVEL && !self.is_cave(x, s, s) {
                    return x;
                }
            }
        }
        0
    }

    /// How steeply the overworld surface rises or falls across this column,
    /// measured as the absolute height change over [`SLOPE_SPAN`] cells either
    /// side. Near zero on flat plains, large on a mountain face. Used to decide
    /// whether a cave may open through the surface here.
    fn surface_slope(&self, world_x: i32) -> i32 {
        (self.surface_height(world_x + SLOPE_SPAN) - self.surface_height(world_x - SLOPE_SPAN))
            .abs()
    }

    /// The block to place at `world_y` in a column whose surface is at `surface`
    /// and that belongs to `biome`. `world_y < surface` is air (the caller skips
    /// those cells).
    fn block_at(biome: Biome, surface: i32, world_y: i32) -> crate::protocol::BlockId {
        match biome {
            // Mountains are bare stone from the surface down.
            Biome::Mountains => STONE,
            // The desert is a band of sand over stone — no grass, no dirt.
            Biome::Desert => {
                if world_y <= surface + SAND_DEPTH {
                    SAND
                } else {
                    STONE
                }
            }
            // The ocean floor is a sandy seabed over stone — the same band as the
            // desert, only it lies deep underwater (the column above it is flooded
            // with water by the caller down to this surface).
            Biome::Ocean => {
                if world_y <= surface + SAND_DEPTH {
                    SAND
                } else {
                    STONE
                }
            }
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
    /// contours of two noise fields (which branch where they cross), and large
    /// **caverns** deep down where a low-frequency field peaks. Both are kept
    /// below a solid [`SURFACE_CRUST`], so a tunnel only reaches daylight where
    /// the terrain falls away beside it — a cave mouth in the side of a hill or
    /// mountain, never a hole underfoot on flat ground. Only meaningful for cells
    /// at or below the surface (the caller skips the open air above).
    fn is_cave(&self, world_x: i32, world_y: i32, surface: i32) -> bool {
        // Keep a solid floor at the very bottom of the world.
        if world_y >= WORLD_HEIGHT - BEDROCK_FLOOR {
            return false;
        }
        // Within the surface crust, only carve through a steep hillside, so cave
        // mouths open in the side of a hill or mountain and never as a pit in
        // flat ground. Below the crust, tunnels and caverns carve freely.
        if world_y - surface < SURFACE_CRUST && self.surface_slope(world_x) < STEEP_THRESHOLD {
            return false;
        }
        let (x, y) = (world_x as f32, world_y as f32);
        // Winding tunnels: carve a band either side of the field's zero contour.
        // Below the crust, these open to daylight only on an exposed slope.
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
                    // Above the ground: flood a submerged column with water from
                    // the sea surface down to the sea floor, leaving everything
                    // higher as open air.
                    if world_y >= SEA_LEVEL {
                        chunk.set(lx, ly, WATER);
                    }
                    continue;
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
        // Rare embedded ruins rest on the surface, placed before trees so a stone
        // ruin takes precedence and trees grow around it (both fill only air).
        // Scan a margin wide enough to catch any whose footprint reaches into this
        // chunk from the left, and clip like trees. Skip submerged or cave-mouth
        // columns so a ruin never floats over water or a hole.
        if let Some(struct_w) = self.structure.as_ref().map(|s| s.width as i32) {
            for world_x in (base_x - struct_w)..(base_x + CHUNK_SIZE) {
                let surface = self.surface_height(world_x);
                if self.structure_root_at(world_x)
                    && surface <= SEA_LEVEL
                    && !self.is_cave(world_x, surface, surface)
                {
                    self.place_structure(&mut chunk, base_x, base_y, world_x);
                }
            }
        }
        // Trees grow upward from the surface and can lean their canopies across
        // chunk borders, so scan a margin of columns either side and let
        // [`place_tree`] clip whatever falls outside this chunk. Skip columns
        // whose surface a cave has opened, so trees don't float over cave mouths.
        for world_x in (base_x - CANOPY_RADIUS)..(base_x + CHUNK_SIZE + CANOPY_RADIUS) {
            let surface = self.surface_height(world_x);
            // No trees on submerged ground: a flooded column would leave a trunk
            // sprouting out of (or floating over) the water.
            if self.tree_root_at(world_x)
                && surface <= SEA_LEVEL
                && !self.is_cave(world_x, surface, surface)
            {
                self.place_tree(&mut chunk, base_x, base_y, world_x);
            }
        }
        chunk
    }

    /// Generate a chunk for the given [`Dimension`]. The overworld uses
    /// [`generate_chunk`](Self::generate_chunk); the underworld is built from
    /// charred rock by [`generate_underworld_chunk`](Self::generate_underworld_chunk).
    pub fn generate(&self, dim: Dimension, cx: i32, cy: i32) -> Chunk {
        match dim {
            Dimension::Overworld => self.generate_chunk(cx, cy),
            Dimension::Underworld => self.generate_underworld_chunk(cx, cy),
        }
    }

    /// Floor row (topmost solid charred rock the player walks on) of the
    /// underworld for a world column. The open cavern rises above it up to the
    /// [`underworld_ceiling`](Self::underworld_ceiling); the charred depths fall
    /// away below it to bedrock.
    pub fn underworld_surface(&self, world_x: i32) -> i32 {
        let n = self.uw_height_noise.get_noise_2d(world_x as f32, 0.0); // -1..1
        (UNDERWORLD_SURFACE_BASE as f32 + n * UNDERWORLD_SURFACE_AMP).round() as i32
    }

    /// Ceiling row of the underworld for a world column: solid charred rock fills
    /// `0..row` (the roof), and the topmost open cavern cell is at `row`. Undulates
    /// gently per column — offset off the floor's noise so roof and floor aren't
    /// mirror images — so the visible ceiling reads as natural rock.
    pub fn underworld_ceiling(&self, world_x: i32) -> i32 {
        let n = self
            .uw_height_noise
            .get_noise_2d(world_x as f32 + 1000.0, 0.0); // -1..1
        (UNDERWORLD_CEILING_BASE as f32 + n * UNDERWORLD_CEILING_AMP)
            .round()
            .max(1.0) as i32
    }

    /// How steeply the underworld floor rises or falls across this column,
    /// measured like [`surface_slope`](Self::surface_slope). Used to decide
    /// whether a cave may open through the charred floor here.
    fn underworld_slope(&self, world_x: i32) -> i32 {
        (self.underworld_surface(world_x + SLOPE_SPAN)
            - self.underworld_surface(world_x - SLOPE_SPAN))
        .abs()
    }

    /// Surface row for `dim`'s spawn/landing helpers: the overworld grass line or
    /// the underworld's charred floor (the cavern floor a warped-in player lands on).
    pub fn surface_for(&self, dim: Dimension, world_x: i32) -> i32 {
        match dim {
            Dimension::Overworld => self.surface_height(world_x),
            Dimension::Underworld => self.underworld_surface(world_x),
        }
    }

    /// Whether the charred cell at `(world_x, world_y)` below the floor is hollowed
    /// into a cave. Mirrors the overworld [`is_cave`](WorldGen::is_cave): thin
    /// **winding tunnels** along the zero contour of one field, plus large
    /// **caverns** deep down where a low-frequency field peaks — the same
    /// [`TUNNEL_WIDTH`], [`CAVERN_THRESHOLD`], and [`CAVERN_MIN_DEPTH`], just on
    /// the underworld's own seeds. The [`SURFACE_CRUST`] below the charred floor
    /// is carved only where the floor drops away steeply, so caves open into the
    /// cavern on the side of a ledge — never as a hole in the floor the player
    /// lands on. The bottom-most rows are spared too, so nothing opens into the
    /// void below the world.
    fn is_underworld_cave(&self, world_x: i32, world_y: i32, surface: i32) -> bool {
        if world_y >= WORLD_HEIGHT - BEDROCK_FLOOR {
            return false;
        }
        // As in the overworld, keep a solid crust beneath the charred floor except
        // where it drops away steeply, so caves open in the side of a ledge and
        // never as a hole in the floor the player lands on.
        if world_y - surface < SURFACE_CRUST && self.underworld_slope(world_x) < STEEP_THRESHOLD {
            return false;
        }
        let (x, y) = (world_x as f32, world_y as f32);
        // Winding tunnels: carve a band either side of the field's zero contour.
        if self.uw_cave_noise.get_noise_2d(x, y).abs() < TUNNEL_WIDTH {
            return true;
        }
        // Deep caverns: big open rooms, only well below the charred floor.
        if world_y - surface >= CAVERN_MIN_DEPTH
            && self.uw_cavern_noise.get_noise_2d(x, y) > CAVERN_THRESHOLD
        {
            return true;
        }
        false
    }

    /// Whether the underworld cell at `(world_x, world_y)` is solid charred rock.
    /// Three bands stack down a column: the solid **roof** (`0..ceiling`), the open
    /// **cavern** (`ceiling..floor`), and the solid charred **depths**
    /// (`floor..`) — the last riddled with caves.
    fn underworld_solid(&self, world_x: i32, world_y: i32, ceiling: i32, floor: i32) -> bool {
        if world_y < ceiling {
            return true; // the roof: solid charred rock from the top of the world
        }
        if world_y < floor {
            return false; // the open cavern between roof and floor
        }
        // The charred depths below the floor, hollowed by caves.
        !self.is_underworld_cave(world_x, world_y, floor)
    }

    /// Which underworld biome the given world column belongs to. Picks an ash
    /// valley where the (shared) biome-noise field dips low, leaving the rest as
    /// the default charred expanse.
    pub fn underworld_biome_at(&self, world_x: i32) -> UnderworldBiome {
        let n = self.biome_noise.get_noise_2d(world_x as f32, 0.0); // -1..1
        if n < ASH_VALLEY_THRESHOLD {
            UnderworldBiome::AshValley
        } else {
            UnderworldBiome::Charred
        }
    }

    /// Which block a solid underworld cell becomes, given the column's `floor`
    /// row: a band of [`ASH`] over the floor of an ash valley, a vein of
    /// [`TUNGSTEN_ORE`] where the tungsten field peaks well below the floor, and
    /// plain charred rock everywhere else (including the whole roof, which sits
    /// above the floor and so matches neither band).
    fn underworld_block(&self, world_x: i32, world_y: i32, floor: i32) -> BlockId {
        let depth = world_y - floor;
        // Ash valleys blanket their charred-rock floor in a band of loose ash,
        // shallower than tungsten ever sits, so the two never conflict. Guarded to
        // the floor band so the roof (negative depth) stays charred rock.
        if (0..ASH_DEPTH).contains(&depth)
            && self.underworld_biome_at(world_x) == UnderworldBiome::AshValley
        {
            return ASH;
        }
        if depth >= TUNGSTEN_ORE_MIN_DEPTH {
            let v = self
                .tungsten_noise
                .get_noise_3d(world_x as f32, world_y as f32, 0.0); // -1..1
            if v > TUNGSTEN_ORE_THRESHOLD {
                return TUNGSTEN_ORE;
            }
        }
        // Gold sits where tungsten didn't claim the cell — shallower and scarcer.
        if depth >= GOLD_ORE_MIN_DEPTH {
            let v = self
                .gold_noise
                .get_noise_3d(world_x as f32, world_y as f32, 0.0); // -1..1
            if v > GOLD_ORE_THRESHOLD {
                return GOLD_ORE;
            }
        }
        CHARRED_ROCK
    }

    /// Generate one charred-rock chunk of the underworld. A solid charred roof caps
    /// the column at the top of the world; beneath it opens a tall cavern; below the
    /// floor, charred rock falls away to bedrock, riddled with caves. A scattering
    /// of natural fire flickers on exposed floors (the cavern floor and cave ledges).
    fn generate_underworld_chunk(&self, cx: i32, cy: i32) -> Chunk {
        let mut chunk = Chunk::empty();
        let base_x = cx * CHUNK_SIZE;
        let base_y = cy * CHUNK_SIZE;
        for lx in 0..CHUNK_SIZE {
            let world_x = base_x + lx;
            let ceiling = self.underworld_ceiling(world_x);
            let floor = self.underworld_surface(world_x);
            for ly in 0..CHUNK_SIZE {
                let world_y = base_y + ly;
                if self.underworld_solid(world_x, world_y, ceiling, floor) {
                    chunk.set(lx, ly, self.underworld_block(world_x, world_y, floor));
                    continue;
                }
                // Open cell (cavern or cave): kindle a flame on any floor it exposes
                // — a charred-rock cell directly beneath it — by a sparse roll. The
                // roof's underside has open air below it, so it never catches fire.
                if self.underworld_solid(world_x, world_y + 1, ceiling, floor)
                    && self.cell_hash(world_x, world_y, 0xF12E) % 1000 < UNDERWORLD_FIRE_CHANCE
                {
                    chunk.set(lx, ly, FIRE);
                }
            }
        }
        chunk
    }

    /// Deterministic pseudo-random value for a single cell, mixed with `salt` so
    /// independent decisions can be drawn from one cell. Seeded so a cell always
    /// decides the same way regardless of who generates it.
    fn cell_hash(&self, world_x: i32, world_y: i32, salt: u32) -> u32 {
        let mut h = (self.seed as u32)
            .wrapping_mul(374_761_393)
            .wrapping_add((world_x as u32).wrapping_mul(668_265_263))
            .wrapping_add((world_y as u32).wrapping_mul(2_246_822_519));
        h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
        h = h.wrapping_add(salt.wrapping_mul(0x9E37_79B9));
        h ^ (h >> 16)
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
            // Bare stone mountains, barren deserts, and open ocean grow no trees.
            Biome::Mountains | Biome::Desert | Biome::Ocean => 0,
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

    /// Whether a lowland column rolls an embedded structure. Restricted to the
    /// grassy biomes, on its own hash salt so it's independent of tree placement.
    fn column_rolls_structure(&self, world_x: i32) -> bool {
        matches!(self.biome_at(world_x), Biome::Plains | Biome::Forest)
            && self.col_hash(world_x, 0x57) % 1000 < STRUCTURE_CHANCE
    }

    /// Whether a structure is rooted (anchored at its left edge) at this column.
    /// Suppressed if the left neighbour also rolled, so two never overlap edges.
    fn structure_root_at(&self, world_x: i32) -> bool {
        self.column_rolls_structure(world_x) && !self.column_rolls_structure(world_x - 1)
    }

    /// Stamp the embedded structure's blocks with its left edge at `root_x`,
    /// resting on the surface and rising into the air. Clips to the chunk and only
    /// fills air, so it never gouges terrain (matching [`place_tree`](Self::place_tree)).
    /// Block data only — terrain generation is pure, so any captured entities in
    /// the structure are not spawned here (the embedded ruin has none).
    fn place_structure(&self, chunk: &mut Chunk, base_x: i32, base_y: i32, root_x: i32) {
        let Some(s) = &self.structure else {
            return;
        };
        let surface = self.surface_height(root_x);
        let top = surface - s.height as i32; // bottom row sits just above the ground
        for (dx, dy, b) in s.solid_offsets() {
            put_block(chunk, base_x, base_y, root_x + dx, top + dy, b);
        }
    }

    /// The creatures an embedded structure wants spawned in overworld chunk
    /// `(cx, cy)`, each as `(world_x_px, world_y_px, root_x, kind)`. `root_x`
    /// identifies the structure so the server can realize its creatures exactly
    /// once. A root is reported only by the chunk holding the structure's base
    /// row, so the creatures appear with the ruin (and near the player) rather
    /// than when some far-off column chunk happens to load first. Empty when the
    /// embedded structure carries no entities.
    pub fn structure_entities_in_chunk(
        &self,
        cx: i32,
        cy: i32,
    ) -> Vec<(f32, f32, i32, EntityKind)> {
        let mut out = Vec::new();
        let Some(s) = &self.structure else {
            return out;
        };
        if s.entities.is_empty() {
            return out;
        }
        let base_x = cx * CHUNK_SIZE;
        for root_x in base_x..base_x + CHUNK_SIZE {
            if !self.structure_root_at(root_x) {
                continue;
            }
            let surface = self.surface_height(root_x);
            if surface > SEA_LEVEL || self.is_cave(root_x, surface, surface) {
                continue;
            }
            if (surface - 1).div_euclid(CHUNK_SIZE) != cy {
                continue;
            }
            let top = surface - s.height as i32;
            let ox = root_x as f32 * TILE_SIZE;
            let oy = top as f32 * TILE_SIZE;
            for se in &s.entities {
                out.push((ox + se.dx, oy + se.dy, root_x, se.kind.clone()));
            }
        }
        out
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
    use crate::block::{AIR, IRON_ORE, WATER};

    /// Low-lying basins flood with water, and that water only ever sits at or
    /// below the sea surface — never spilling up onto open air above it.
    #[test]
    fn submerged_basins_flood_up_to_sea_level() {
        let worldgen = WorldGen::new(0xC0FFEE);
        let mut water = 0u32;
        for cx in -8..8 {
            for cy in 0..(WORLD_HEIGHT / CHUNK_SIZE) {
                let chunk = worldgen.generate_chunk(cx, cy);
                for lx in 0..CHUNK_SIZE {
                    for ly in 0..CHUNK_SIZE {
                        if chunk.get(lx, ly) == WATER {
                            water += 1;
                            let world_y = cy * CHUNK_SIZE + ly;
                            assert!(
                                world_y >= SEA_LEVEL,
                                "water at row {world_y} sits above sea level {SEA_LEVEL}"
                            );
                        }
                    }
                }
            }
        }
        assert!(water > 0, "expected some basins to flood with water");
    }

    /// An ocean biome generates somewhere, and where it does the column is a deep
    /// body of open water over a sandy seabed: the floor sits well below sea level,
    /// the cells from sea level down to the floor are all water, and the floor cell
    /// itself is sand.
    #[test]
    fn ocean_biome_is_deep_water_over_a_sand_floor() {
        use crate::block::SAND;
        use crate::world::to_chunk;

        let worldgen = WorldGen::new(0xC0FFEE);
        // Find an ocean column; oceans are broad, so a wide scan must hit one.
        let x = (-8000..8000)
            .find(|&x| worldgen.biome_at(x) == Biome::Ocean)
            .expect("an ocean should generate somewhere across 16000 columns");

        // The seabed sits deep below the waterline (larger row = lower down).
        let floor = worldgen.surface_height(x);
        assert!(
            floor > SEA_LEVEL,
            "ocean floor at row {floor} should be below sea level {SEA_LEVEL}"
        );

        // Every cell from the sea surface down to just above the floor is water.
        let block_at = |wx: i32, wy: i32| {
            let ((cx, cy), (lx, ly)) = to_chunk(wx, wy);
            worldgen.generate_chunk(cx, cy).get(lx, ly)
        };
        for wy in SEA_LEVEL..floor {
            assert_eq!(block_at(x, wy), WATER, "ocean column dry at row {wy}");
        }
        // The seabed the water rests on is sand.
        assert_eq!(block_at(x, floor), SAND, "ocean floor should be sand");
    }

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

    /// The embedded ruin parses and is actually stamped onto the surface: the
    /// first rooted column over open land has a stone wall sitting in what would
    /// otherwise be air above the natural ground.
    #[test]
    fn embedded_structures_are_placed_on_the_surface() {
        use crate::block::STONE;
        use crate::world::to_chunk;

        let worldgen = WorldGen::new(0xC0FFEE);
        assert!(worldgen.structure.is_some(), "embedded ruin should parse");

        // Find an open lowland column that roots a ruin, scanning a wide span.
        let root = (-4000..4000)
            .find(|&x| {
                let s = worldgen.surface_height(x);
                worldgen.structure_root_at(x) && s <= SEA_LEVEL && !worldgen.is_cave(x, s, s)
            })
            .expect("a ruin should root somewhere across 8000 columns");

        // The ruin's left wall is stone; a cell two rows above the surface is in
        // the air for natural terrain, so finding stone there proves it was placed.
        let probe_y = worldgen.surface_height(root) - 2;
        let ((cx, cy), (lx, ly)) = to_chunk(root, probe_y);
        let chunk = worldgen.generate_chunk(cx, cy);
        assert_eq!(
            chunk.get(lx, ly),
            STONE,
            "ruin wall missing above the surface at column {root}"
        );
    }

    /// The embedded ruin carries a guardian slime, reported only by the chunk
    /// holding the structure's base (so the server spawns it with the ruin).
    #[test]
    fn embedded_structure_reports_entities_for_its_base_chunk() {
        use crate::world::to_chunk;

        let worldgen = WorldGen::new(0xC0FFEE);
        assert!(
            worldgen
                .structure
                .as_ref()
                .is_some_and(|s| !s.entities.is_empty()),
            "embedded ruin should carry at least one creature"
        );

        let root = (-4000..4000)
            .find(|&x| {
                let s = worldgen.surface_height(x);
                worldgen.structure_root_at(x) && s <= SEA_LEVEL && !worldgen.is_cave(x, s, s)
            })
            .expect("a ruin should root somewhere across 8000 columns");
        let surface = worldgen.surface_height(root);
        let ((ocx, ocy), _) = to_chunk(root, surface - 1);

        // The base chunk reports the slime for this root.
        let here = worldgen.structure_entities_in_chunk(ocx, ocy);
        assert!(
            here.iter()
                .any(|(_, _, r, k)| *r == root && *k == EntityKind::Slime),
            "base chunk should report the ruin's slime"
        );
        // A chunk well above the surface reports nothing for this root.
        let above = worldgen.structure_entities_in_chunk(ocx, ocy - 3);
        assert!(above.iter().all(|(_, _, r, _)| *r != root));
    }

    /// The underworld stacks a solid charred roof, a tall open cavern, and a
    /// charred floor that — riddled with caves and dotted with fire — falls away to
    /// a solid bedrock floor at the very bottom. Scanned over a wide span so the
    /// low-frequency ash-valley field is sure to be sampled.
    #[test]
    fn underworld_is_charred_rock_with_fire_and_a_floor() {
        use crate::world::Dimension;
        let worldgen = WorldGen::new(0xC0FFEE);
        let (mut charred, mut fire, mut air, mut tungsten, mut gold, mut ash) =
            (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
        for cx in -32..32 {
            for cy in 0..(WORLD_HEIGHT / CHUNK_SIZE) {
                let chunk = worldgen.generate(Dimension::Underworld, cx, cy);
                for lx in 0..CHUNK_SIZE {
                    for ly in 0..CHUNK_SIZE {
                        match chunk.get(lx, ly) {
                            CHARRED_ROCK => charred += 1,
                            FIRE => fire += 1,
                            AIR => air += 1,
                            TUNGSTEN_ORE => tungsten += 1,
                            GOLD_ORE => gold += 1,
                            ASH => ash += 1,
                            other => panic!("unexpected underworld block {other}"),
                        }
                    }
                }
            }
        }
        assert!(charred > 0, "expected charred rock to fill the underworld");
        // Ash valleys blanket part of the underworld surface in loose ash.
        assert!(ash > 0, "expected ash valleys to lay down some ash");
        assert!(fire > 0, "expected some natural fire in the underworld");
        assert!(air > 0, "expected open air (the cavern and its caves)");
        // Tungsten ore is the underworld's reward: present, but rarer than its rock.
        assert!(
            tungsten > 0,
            "expected tungsten ore veins in the underworld"
        );
        assert!(
            tungsten < charred,
            "tungsten ore should be rarer than charred rock"
        );
        // Gold ore is the underworld's other reward: present, but rarer than rock.
        assert!(gold > 0, "expected gold ore veins in the underworld");
        assert!(gold < charred, "gold ore should be rarer than charred rock");
        // Charred rock dominates: the underworld is mostly solid stone.
        assert!(charred > air, "underworld carved too hollow");

        // The bottom rows are a solid floor — charred rock or a tungsten vein, but
        // never carved open into the void.
        let bottom_cy = WORLD_HEIGHT / CHUNK_SIZE - 1;
        for cx in -4..4 {
            let chunk = worldgen.generate(Dimension::Underworld, cx, bottom_cy);
            for lx in 0..CHUNK_SIZE {
                let cell = chunk.get(lx, CHUNK_SIZE - 1);
                assert!(
                    cell == CHARRED_ROCK || cell == TUNGSTEN_ORE || cell == GOLD_ORE,
                    "underworld floor carved open at chunk {cx}, column {lx}"
                );
            }
        }
    }

    /// Caves only break the surface on a steep slope — the side of a hill or
    /// mountain — never as a pit in flat ground. Every column whose surface cell
    /// is carved open (a cave mouth) is a steep face, and some such mouths exist.
    #[test]
    fn caves_open_only_on_steep_faces() {
        let worldgen = WorldGen::new(0xC0FFEE);
        let mut mouths = 0u32;
        for x in -4000..4000 {
            let s = worldgen.surface_height(x);
            if worldgen.is_cave(x, s, s) {
                mouths += 1;
                assert!(
                    worldgen.surface_slope(x) >= STEEP_THRESHOLD,
                    "cave opened on flat ground (slope {}) at column {x}",
                    worldgen.surface_slope(x),
                );
            }
        }
        assert!(
            mouths > 0,
            "expected some caves to open on hill and mountain faces",
        );
    }

    /// The same holds in the underworld: no cave opens a hole in the charred
    /// ceiling the player lands on — every ceiling mouth sits on a steep ledge.
    #[test]
    fn underworld_caves_open_only_on_steep_faces() {
        let worldgen = WorldGen::new(0xC0FFEE);
        for x in -4000..4000 {
            let s = worldgen.underworld_surface(x);
            if worldgen.is_underworld_cave(x, s, s) {
                assert!(
                    worldgen.underworld_slope(x) >= STEEP_THRESHOLD,
                    "underworld cave opened in the flat ceiling (slope {}) at column {x}",
                    worldgen.underworld_slope(x),
                );
            }
        }
    }
}
