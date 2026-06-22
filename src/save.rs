//! Binary persistence of world state to `.dat` files.
//!
//! A saved world lives in its own directory:
//!
//! ```text
//! <world>/
//!   world.dat            metadata: seed, clock, id counter, spawn, entities, players
//!   chunks/
//!     <cx>_<cy>.dat      one file per generated-and-modified chunk
//! ```
//!
//! Everything is binary (not text): `world.dat` is a small magic/version header
//! followed by a `bincode` blob, and each chunk is the raw little-endian
//! [`BlockId`] grid (`CHUNK_AREA * 2` bytes, fixed). Binary keeps saves compact
//! and quick to load.
//!
//! Only *modified* chunks are written. Untouched terrain is reproduced exactly
//! from the saved [`seed`](WorldMeta::seed) by [`crate::worldgen`], which is
//! deterministic, so there is no loss in skipping it.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::entity::Entity;
use crate::inventory::Inventory;
use crate::protocol::BlockId;
use crate::world::{CHUNK_AREA, Chunk, ChunkCoord};

/// Name of the metadata file inside a world directory.
const WORLD_FILE: &str = "world.dat";
/// Subdirectory holding per-chunk block data.
const CHUNKS_DIR: &str = "chunks";
/// Magic prefix on `world.dat` ("SCWD" — Survival Cubed World Data).
const MAGIC: u32 = 0x5343_5744;
/// On-disk format version; bump on any incompatible layout change.
const VERSION: u32 = 5;
/// Bytes per chunk file: one little-endian `u16` per cell.
const CHUNK_BYTES: usize = CHUNK_AREA * 2;

/// Per-player state persisted across sessions, keyed by display name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPlayer {
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub health: i32,
    /// The player's slot inventory, so collected blocks (and how they're
    /// arranged) survive disconnects and restarts.
    pub inventory: Inventory,
    /// Cell of the last campfire the player interacted with, or `None` to fall back
    /// to world spawn. Persisted so a death after a reconnect still returns the
    /// player to their fire (provided it's still standing).
    #[serde(default)]
    pub respawn: Option<(i32, i32)>,
}

/// Top-level world metadata stored in `world.dat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldMeta {
    /// Generator seed. Reused on load so unexplored terrain stays consistent.
    pub seed: i32,
    /// Seconds of in-world time elapsed; drives the day/night clock on resume.
    pub elapsed_secs: f32,
    /// Next entity id to allocate, so loaded ids never collide with new ones.
    pub next_id: u32,
    /// Player spawn point in world pixels.
    pub spawn: (f32, f32),
    /// Server-simulated creatures (slimes, …). Players are stored separately.
    pub entities: Vec<Entity>,
    /// Saved state of every player who has ever joined this world.
    pub players: Vec<SavedPlayer>,
    /// Lit campfires as `(x, y, remaining_burn_secs)`, so fires keep burning
    /// across a save/reload instead of staying lit forever or going dark.
    #[serde(default)]
    pub campfires: Vec<(i32, i32, f32)>,
    /// Cells holding a player-placed log, as `(x, y)`. Tracked so an axe's
    /// tree-felling spares logs the player built with (see [`crate::server`]).
    #[serde(default)]
    pub placed_logs: Vec<(i32, i32)>,
}

/// Reads and writes a single world's files under `dir`.
pub struct WorldStore {
    dir: PathBuf,
}

impl WorldStore {
    pub fn new(dir: PathBuf) -> Self {
        WorldStore { dir }
    }

    fn meta_path(&self) -> PathBuf {
        self.dir.join(WORLD_FILE)
    }

    fn chunks_dir(&self) -> PathBuf {
        self.dir.join(CHUNKS_DIR)
    }

    fn chunk_path(&self, (cx, cy): ChunkCoord) -> PathBuf {
        self.chunks_dir().join(format!("{cx}_{cy}.dat"))
    }

    /// Load world metadata, or `None` if this world has never been saved.
    pub fn load_meta(&self) -> Result<Option<WorldMeta>> {
        let path = self.meta_path();
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if bytes.len() < 8 {
            bail!("{} is truncated", path.display());
        }
        let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if magic != MAGIC {
            bail!("{} is not a Survival Cubed world", path.display());
        }
        if version != VERSION {
            bail!(
                "{} is save version {version}, expected {VERSION}",
                path.display()
            );
        }
        let meta = bincode::deserialize(&bytes[8..])
            .with_context(|| format!("decoding {}", path.display()))?;
        Ok(Some(meta))
    }

    /// Write world metadata, replacing any previous copy atomically.
    pub fn save_meta(&self, meta: &WorldMeta) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("creating {}", self.dir.display()))?;
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend(bincode::serialize(meta).context("encoding world metadata")?);
        write_atomic(&self.meta_path(), &out)
    }

    /// Load a previously-saved chunk, or `None` if it has never been written.
    pub fn load_chunk(&self, coord: ChunkCoord) -> Result<Option<Chunk>> {
        let path = self.chunk_path(coord);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if bytes.len() != CHUNK_BYTES {
            bail!(
                "{} is {} bytes, expected {CHUNK_BYTES}",
                path.display(),
                bytes.len()
            );
        }
        let blocks: Vec<BlockId> = bytes
            .chunks_exact(2)
            .map(|p| BlockId::from_le_bytes([p[0], p[1]]))
            .collect();
        Ok(Some(Chunk::from_vec(blocks)))
    }

    /// Write a chunk's blocks as a fixed-size little-endian grid.
    pub fn save_chunk(&self, coord: ChunkCoord, chunk: &Chunk) -> Result<()> {
        let dir = self.chunks_dir();
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let mut bytes = Vec::with_capacity(CHUNK_BYTES);
        for b in chunk.blocks.iter() {
            bytes.extend_from_slice(&b.to_le_bytes());
        }
        write_atomic(&self.chunk_path(coord), &bytes)
    }
}

/// Write `bytes` to `path` via a temporary file + rename, so a crash mid-write
/// can never leave a half-written save in place.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("replacing {} with {}", path.display(), tmp.display()))?;
    Ok(())
}

/// The directory holding every world's save folder.
fn saves_dir() -> PathBuf {
    let mut p = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("survival-cubed");
    p.push("saves");
    p
}

/// Directory for a named world under the platform data dir, e.g.
/// `~/.local/share/survival-cubed/saves/<name>`. Falls back to `./saves/<name>`
/// if no data dir is known.
pub fn world_dir(name: &str) -> PathBuf {
    let mut p = saves_dir();
    p.push(name);
    p
}

/// Whether a world with this name has already been saved.
pub fn world_exists(name: &str) -> bool {
    world_dir(name).join(WORLD_FILE).exists()
}

/// A saved world found on disk, summarised for the menu's world picker.
pub struct WorldInfo {
    /// Directory name, which doubles as the world's identifier.
    pub name: String,
    /// Generator seed read from the world's metadata.
    pub seed: i32,
}

/// List every saved world, newest-looking ordering aside, sorted by name. Only
/// directories with a readable `world.dat` are returned; anything unreadable is
/// silently skipped so a single corrupt save can't hide the rest.
pub fn list_worlds() -> Vec<WorldInfo> {
    let mut worlds = Vec::new();
    let entries = match fs::read_dir(saves_dir()) {
        Ok(e) => e,
        Err(_) => return worlds,
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let store = WorldStore::new(entry.path());
        if let Ok(Some(meta)) = store.load_meta() {
            worlds.push(WorldInfo {
                name,
                seed: meta.seed,
            });
        }
    }
    worlds.sort_by(|a, b| a.name.cmp(&b.name));
    worlds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{DIRT, GRASS, STONE};
    use crate::entity::{Entity, EntityKind};

    /// A unique scratch directory for one test, removed when the guard drops.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("scubed-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            TempDir(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn meta_round_trips_without_loss() {
        let tmp = TempDir::new("meta");
        let store = WorldStore::new(tmp.0.clone());
        assert!(store.load_meta().unwrap().is_none(), "no save yet");

        let meta = WorldMeta {
            seed: -424242,
            elapsed_secs: 1234.5,
            next_id: 99,
            spawn: (16.0, -32.0),
            entities: vec![Entity::new(7, EntityKind::Slime, 1.5, 2.5)],
            players: vec![SavedPlayer {
                name: "ada".into(),
                x: 10.0,
                y: 20.0,
                health: 13,
                inventory: {
                    let mut inv = Inventory::new();
                    inv.add(STONE, 42);
                    inv.add(DIRT, 7);
                    inv
                },
                respawn: Some((3, -5)),
            }],
            campfires: vec![(3, -5, 12.5), (-8, 2, 30.0)],
            placed_logs: vec![(1, 2), (-3, 4)],
        };
        store.save_meta(&meta).unwrap();

        let got = store.load_meta().unwrap().expect("save exists");
        assert_eq!(got.seed, meta.seed);
        assert_eq!(got.elapsed_secs, meta.elapsed_secs);
        assert_eq!(got.next_id, meta.next_id);
        assert_eq!(got.spawn, meta.spawn);
        assert_eq!(got.entities.len(), 1);
        assert_eq!(got.entities[0].id, 7);
        assert_eq!(got.entities[0].health, meta.entities[0].health);
        assert_eq!(got.players.len(), 1);
        assert_eq!(got.players[0].name, "ada");
        assert_eq!(got.players[0].health, 13);
        assert_eq!(got.players[0].inventory.get(0), Some((STONE, 42, 0)));
        assert_eq!(got.players[0].inventory.get(1), Some((DIRT, 7, 0)));
        assert_eq!(got.players[0].respawn, Some((3, -5)));
        assert_eq!(got.campfires, meta.campfires);
        assert_eq!(got.placed_logs, meta.placed_logs);
    }

    #[test]
    fn chunk_round_trips_every_cell() {
        let tmp = TempDir::new("chunk");
        let store = WorldStore::new(tmp.0.clone());
        assert!(store.load_chunk((3, -4)).unwrap().is_none());

        // Fill every cell with a distinct, recognizable pattern.
        let palette = [crate::block::AIR, STONE, DIRT, GRASS];
        let mut blocks = vec![0u16; CHUNK_AREA];
        for (i, b) in blocks.iter_mut().enumerate() {
            *b = palette[i % palette.len()];
        }
        let chunk = Chunk::from_vec(blocks.clone());
        store.save_chunk((3, -4), &chunk).unwrap();

        let got = store.load_chunk((3, -4)).unwrap().expect("chunk exists");
        for (i, expected) in blocks.iter().enumerate() {
            assert_eq!(got.blocks[i], *expected, "cell {i} differs");
        }
    }
}
