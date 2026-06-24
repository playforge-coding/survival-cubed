//! Saved block structures: a rectangular stamp of [`BlockId`]s a creator selects
//! and saves, reloads elsewhere, or that worldgen embeds and scatters across the
//! surface.
//!
//! The on-disk (and embedded) format is a tiny binary blob — a magic/version
//! header followed by the width, height, and a row-major little-endian `u16`
//! grid — so the exact bytes a creator saves can be dropped into
//! `assets/structures/` and [`include_bytes!`](std::include_bytes)-embedded for
//! [`crate::worldgen`] to place.

use anyhow::{Result, ensure};

use crate::block::AIR;
use crate::protocol::BlockId;

/// Magic prefix on a structure file ("SCST" — Survival Cubed STructure).
const MAGIC: u32 = 0x5343_5354;
/// On-disk format version; bump on any incompatible layout change.
const VERSION: u32 = 1;
/// Largest width or height a structure may have, bounding memory and the size of
/// the placement message it turns into.
pub const MAX_DIM: u16 = 256;

/// A rectangle of blocks, stored row-major (`y * width + x`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Structure {
    pub width: u16,
    pub height: u16,
    /// Length is exactly `width * height`.
    pub blocks: Vec<BlockId>,
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

    /// Capture the inclusive world-cell region `[x0..=x1] × [y0..=y1]` by sampling
    /// every cell with `sample`. The two corners may be given in any order.
    pub fn from_region(
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        mut sample: impl FnMut(i32, i32) -> BlockId,
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
        Self {
            width,
            height,
            blocks,
        }
    }

    /// Serialize to the binary on-disk / embedded format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + self.blocks.len() * 2);
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        for b in &self.blocks {
            out.extend_from_slice(&b.to_le_bytes());
        }
        out
    }

    /// Parse the binary format, rejecting a bad magic, an unknown version, or a
    /// body whose length doesn't match the declared dimensions.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() >= 12, "structure file too short");
        let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        ensure!(magic == MAGIC, "not a structure file (bad magic)");
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        ensure!(
            version == VERSION,
            "unsupported structure version {version}"
        );
        let width = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
        let height = u16::from_le_bytes(bytes[10..12].try_into().unwrap());
        let count = width as usize * height as usize;
        let body = &bytes[12..];
        ensure!(body.len() == count * 2, "structure body length mismatch");
        let blocks = body
            .chunks_exact(2)
            .map(|p| BlockId::from_le_bytes([p[0], p[1]]))
            .collect();
        Ok(Self {
            width,
            height,
            blocks,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{AIR, STONE};

    #[test]
    fn round_trips_through_bytes() {
        let s = Structure {
            width: 3,
            height: 2,
            blocks: vec![STONE, AIR, STONE, AIR, STONE, AIR],
        };
        let got = Structure::from_bytes(&s.to_bytes()).unwrap();
        assert_eq!(got, s);
    }

    #[test]
    fn from_region_orders_corners_and_samples_row_major() {
        // Corners given bottom-right first; sample encodes the cell as x + y*10.
        let s = Structure::from_region(2, 3, 0, 1, |x, y| (x + y * 10) as BlockId);
        assert_eq!((s.width, s.height), (3, 3));
        assert_eq!(s.get(0, 0), 10); // top-left is (0, 1)
        assert_eq!(s.get(2, 2), 32); // bottom-right is (2, 3)
    }

    #[test]
    fn solid_offsets_skips_air() {
        let s = Structure {
            width: 2,
            height: 1,
            blocks: vec![AIR, STONE],
        };
        let solids: Vec<_> = s.solid_offsets().collect();
        assert_eq!(solids, vec![(1, 0, STONE)]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Structure::from_bytes(b"nope").is_err());
    }
}
