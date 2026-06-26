//! Saved block structures: a rectangular stamp of [`BlockId`]s a creator selects
//! and saves, reloads elsewhere, or that worldgen embeds and scatters across the
//! surface.
//!
//! The on-disk (and embedded) format is a tiny binary blob — a magic/version
//! header, the width and height, a row-major little-endian `u16` block grid, and
//! finally a `bincode` list of the captured entities — so the exact bytes a
//! creator saves can be dropped into `assets/structures/` and
//! [`include_bytes!`](std::include_bytes)-embedded for [`crate::worldgen`] to
//! place. Version-1 files (blocks only, no entity section) still load.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::block::AIR;
use crate::entity::EntityKind;
use crate::protocol::BlockId;
use crate::world::TILE_SIZE;

/// Magic prefix on a structure file ("SCST" — Survival Cubed STructure).
const MAGIC: u32 = 0x5343_5354;
/// On-disk format version. Version 1 is blocks only; version 2 appends the
/// entity section. Readers accept both; writers always emit the latest.
const VERSION: u32 = 2;
/// Largest width or height a structure may have, bounding memory and the size of
/// the placement message it turns into.
pub const MAX_DIM: u16 = 256;

/// One entity captured in a structure: its kind and pixel offset from the
/// structure's top-left cell. Re-spawned relative to the stamp anchor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StructEntity {
    pub dx: f32,
    pub dy: f32,
    pub kind: EntityKind,
}

/// A rectangle of blocks plus the entities captured within it.
#[derive(Clone, Debug, PartialEq)]
pub struct Structure {
    pub width: u16,
    pub height: u16,
    /// Length is exactly `width * height`.
    pub blocks: Vec<BlockId>,
    /// Captured creatures, offset from the top-left cell. May be empty.
    pub entities: Vec<StructEntity>,
}

impl Structure {
    /// The block at structure-local `(x, y)` (`x < width`, `y < height`).
    pub fn get(&self, x: u16, y: u16) -> BlockId {
        self.blocks[y as usize * self.width as usize + x as usize]
    }

    /// Whether this structure has no cells (a degenerate selection).
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Capture the inclusive world-cell region `[x0..=x1] × [y0..=y1]`: sample
    /// every cell with `sample`, and keep the `world_entities` (given in world
    /// pixels as `(x, y, kind)`) whose position falls inside the region, stored as
    /// pixel offsets from the top-left. Corners may be given in any order.
    pub fn from_region(
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        mut sample: impl FnMut(i32, i32) -> BlockId,
        world_entities: impl IntoIterator<Item = (f32, f32, EntityKind)>,
    ) -> Self {
        let (lx, hx) = (x0.min(x1), x0.max(x1));
        let (ly, hy) = (y0.min(y1), y0.max(y1));
        // Clamp the span as i32 before the cast so an enormous drag can't wrap the
        // u16; oversized selections are truncated to MAX_DIM from the top-left.
        let width = (hx - lx + 1).clamp(0, MAX_DIM as i32) as u16;
        let height = (hy - ly + 1).clamp(0, MAX_DIM as i32) as u16;
        let mut blocks = Vec::with_capacity(width as usize * height as usize);
        for y in ly..ly + height as i32 {
            for x in lx..lx + width as i32 {
                blocks.push(sample(x, y));
            }
        }
        // Pixel bounds of the (possibly clamped) region; keep entities inside it,
        // stored relative to the top-left so a stamp can re-offset them.
        let ox = lx as f32 * TILE_SIZE;
        let oy = ly as f32 * TILE_SIZE;
        let (rw, rh) = (width as f32 * TILE_SIZE, height as f32 * TILE_SIZE);
        let entities = world_entities
            .into_iter()
            .filter(|&(wx, wy, _)| wx >= ox && wx < ox + rw && wy >= oy && wy < oy + rh)
            .map(|(wx, wy, kind)| StructEntity {
                dx: wx - ox,
                dy: wy - oy,
                kind,
            })
            .collect();
        Self {
            width,
            height,
            blocks,
            entities,
        }
    }

    /// Serialize to the binary on-disk / embedded format (current version).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + self.blocks.len() * 2);
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        for b in &self.blocks {
            out.extend_from_slice(&b.to_le_bytes());
        }
        // Entity section: a bincode-encoded list (an empty list is a few bytes).
        // Serializing a Vec of plain data to memory is infallible in practice.
        out.extend_from_slice(&bincode::serialize(&self.entities).unwrap_or_default());
        out
    }

    /// Parse the binary format, rejecting a bad magic, an unknown version, or a
    /// body too short for the declared dimensions. Version-1 files have no entity
    /// section and load with no entities.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() >= 12, "structure file too short");
        let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        ensure!(magic == MAGIC, "not a structure file (bad magic)");
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        ensure!(
            version == 1 || version == 2,
            "unsupported structure version {version}"
        );
        let width = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
        let height = u16::from_le_bytes(bytes[10..12].try_into().unwrap());
        let count = width as usize * height as usize;
        let body = &bytes[12..];
        ensure!(body.len() >= count * 2, "structure body too short");
        let (block_bytes, rest) = body.split_at(count * 2);
        let blocks = block_bytes
            .chunks_exact(2)
            .map(|p| BlockId::from_le_bytes([p[0], p[1]]))
            .collect();
        let entities = if version >= 2 {
            bincode::deserialize(rest).context("decoding structure entities")?
        } else {
            Vec::new()
        };
        Ok(Self {
            width,
            height,
            blocks,
            entities,
        })
    }

    /// The non-air cells as `(dx, dy, block)` offsets from the structure's
    /// top-left. Air is treated as transparent — skipped — so stamping a structure
    /// overlays it onto the world without punching holes where it has gaps.
    pub fn solid_offsets(&self) -> impl Iterator<Item = (i32, i32, BlockId)> + '_ {
        (0..self.height).flat_map(move |y| {
            (0..self.width).filter_map(move |x| {
                let b = self.get(x, y);
                (b != AIR).then_some((x as i32, y as i32, b))
            })
        })
    }

    /// A sensible cell to rest a loot chest in, as a `(dx, dy)` offset from the
    /// structure's top-left: an interior **air** cell sitting directly on a solid
    /// block (a floor to stand the chest on), preferring the lowest such cell and,
    /// among ties, the one nearest the horizontal center. `None` if the structure
    /// has no floored interior cell (e.g. a solid blob with no room inside). Used by
    /// the server to drop a ruin's loot chest onto its floor when it's realized.
    pub fn chest_offset(&self) -> Option<(i32, i32)> {
        let center = (self.width as f32 - 1.0) / 2.0;
        let mut best: Option<(i32, i32, f32)> = None; // (dx, dy, score); lower score wins
        for y in 0..self.height {
            for x in 0..self.width {
                if self.get(x, y) != AIR {
                    continue;
                }
                // Needs a solid block directly below to rest on (a floor).
                if y + 1 >= self.height || self.get(x, y + 1) == AIR {
                    continue;
                }
                // Prefer lower rows (larger y), then nearer the center.
                let score = -(y as f32) * 100.0 + (x as f32 - center).abs();
                if best.is_none_or(|(_, _, b)| score < b) {
                    best = Some((x as i32, y as i32, score));
                }
            }
        }
        best.map(|(dx, dy, _)| (dx, dy))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{AIR, STONE};

    #[test]
    fn chest_offset_picks_a_floored_interior_cell() {
        // A little 5x5 hut like the embedded ruin: walls of stone, a 3x3 air
        // interior, and a doorway gap in the bottom row.
        //   .###.
        //   #...#
        //   #...#
        //   #...#
        //   ##.##
        let s = Structure {
            width: 5,
            height: 5,
            #[rustfmt::skip]
            blocks: vec![
                AIR,   STONE, STONE, STONE, AIR,
                STONE, AIR,   AIR,   AIR,   STONE,
                STONE, AIR,   AIR,   AIR,   STONE,
                STONE, AIR,   AIR,   AIR,   STONE,
                STONE, STONE, AIR,   STONE, STONE,
            ],
            entities: vec![],
        };
        // The chosen cell is an interior air cell resting on a solid floor: row 3
        // (the lowest interior row), at a corner column over stone (the center
        // column sits over the doorway gap, so it isn't floored).
        let (dx, dy) = s.chest_offset().expect("hut has a floored interior cell");
        assert_eq!(s.get(dx as u16, dy as u16), AIR);
        assert_ne!(s.get(dx as u16, dy as u16 + 1), AIR, "must rest on a floor");
        assert_eq!(dy, 3);
        assert!(dx == 1 || dx == 3);

        // A solid blob with no room inside has nowhere to stand a chest.
        let solid = Structure {
            width: 2,
            height: 2,
            blocks: vec![STONE, STONE, STONE, STONE],
            entities: vec![],
        };
        assert_eq!(solid.chest_offset(), None);
    }

    #[test]
    fn round_trips_blocks_and_entities() {
        let s = Structure {
            width: 3,
            height: 2,
            blocks: vec![STONE, AIR, STONE, AIR, STONE, AIR],
            entities: vec![
                StructEntity {
                    dx: 4.0,
                    dy: -2.5,
                    kind: EntityKind::Slime,
                },
                StructEntity {
                    dx: 17.0,
                    dy: 1.0,
                    kind: EntityKind::Cat {
                        owner: None,
                        sitting: true,
                    },
                },
            ],
        };
        let got = Structure::from_bytes(&s.to_bytes()).unwrap();
        assert_eq!(got, s);
    }

    #[test]
    fn version_1_files_load_with_no_entities() {
        // A hand-built v1 blob: header (magic, version=1, 2x1) + two blocks, no
        // entity section. Must still parse, yielding an empty entity list.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&STONE.to_le_bytes());
        bytes.extend_from_slice(&AIR.to_le_bytes());
        let s = Structure::from_bytes(&bytes).unwrap();
        assert_eq!((s.width, s.height), (2, 1));
        assert_eq!(s.blocks, vec![STONE, AIR]);
        assert!(s.entities.is_empty());
    }

    #[test]
    fn from_region_orders_corners_and_captures_entities() {
        // Corners given bottom-right first; sample encodes the cell as x + y*10.
        // With TILE_SIZE=16, the region [0..=2]×[1..=3] covers pixels [0,48)×[16,64).
        let inside = (20.0, 30.0, EntityKind::Slime); // -> offset (20, 14)
        let outside = (200.0, 30.0, EntityKind::Chicken); // beyond the region
        let s = Structure::from_region(
            2,
            3,
            0,
            1,
            |x, y| (x + y * 10) as BlockId,
            [inside, outside],
        );
        assert_eq!((s.width, s.height), (3, 3));
        assert_eq!(s.get(0, 0), 10); // top-left is (0, 1)
        assert_eq!(s.get(2, 2), 32); // bottom-right is (2, 3)
        assert_eq!(s.entities.len(), 1);
        assert_eq!(s.entities[0].kind, EntityKind::Slime);
        assert_eq!((s.entities[0].dx, s.entities[0].dy), (20.0, 14.0));
    }

    #[test]
    fn solid_offsets_skips_air() {
        let s = Structure {
            width: 2,
            height: 1,
            blocks: vec![AIR, STONE],
            entities: Vec::new(),
        };
        let solids: Vec<_> = s.solid_offsets().collect();
        assert_eq!(solids, vec![(1, 0, STONE)]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Structure::from_bytes(b"nope").is_err());
    }

    /// Dev tool: rewrite the embedded ruin (a stone hut with a guardian slime)
    /// using the real serializer, so its bytes always match the current format.
    /// Run with `cargo test regenerate_embedded_ruin -- --ignored`.
    #[test]
    #[ignore]
    fn regenerate_embedded_ruin() {
        let blocks = vec![
            AIR, STONE, STONE, STONE, AIR, //
            STONE, AIR, AIR, AIR, STONE, //
            STONE, AIR, AIR, AIR, STONE, //
            STONE, AIR, AIR, AIR, STONE, //
            STONE, STONE, AIR, STONE, STONE, //
        ];
        let s = Structure {
            width: 5,
            height: 5,
            blocks,
            // A slime stands on the interior floor (cell (1, 3) -> px (16, 48)).
            entities: vec![StructEntity {
                dx: 16.0,
                dy: 48.0,
                kind: EntityKind::Slime,
            }],
        };
        std::fs::write("assets/structures/ruin.scst", s.to_bytes()).unwrap();
    }
}
