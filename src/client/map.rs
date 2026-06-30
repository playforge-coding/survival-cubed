//! Client-side live map: stream this player's position and explored chunks over
//! the shared [MOQ](https://moq.dev) relay, and receive everyone else's, so the
//! in-game map (full screen on `Tab`, corner minimap on `H`) shows where every
//! player is and the terrain anyone has discovered — even areas this client has
//! never loaded itself.
//!
//! Same shape as [`crate::client::voice`] and [`crate::client::webcam`]: a
//! background thread runs a tokio runtime driving the MOQ session, and the UI
//! talks to it through a cheap [`MapHandle`]. It rides the *same* relay as voice
//! and webcam, kept apart by its own broadcast-path prefix (see
//! [`crate::voice::map_broadcast_path`]).
//!
//! Unlike voice/webcam, the outbound data isn't captured on a thread — it comes
//! from the main game loop, which calls [`MapHandle::send_pos`] and
//! [`MapHandle::send_tile`]. Those enqueue [`MapPacket`]s the publish task drains
//! and writes to our single map track. Each packet is self-contained
//! ([`bincode`]-encoded, wrapped in a [`hang`] frame), so a player subscribing
//! mid-session decodes every future packet on its own; terrain it missed is
//! filled in by the publisher's slow re-broadcast rotor (see the main loop).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use moq_net::bytes::Bytes;
use moq_net::{Origin, Track};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::entity::EntityId;
use crate::protocol::BlockId;
use crate::voice::{MAP_TRACK, map_broadcast_path, map_entity_from_path};
use crate::world::{ChunkCoord, Dimension};

/// How long after a player's last position update its marker is kept on the map.
/// Positions are sent at a steady low rate (see the main loop), so a still player
/// stays live; once nothing arrives for this long the player is assumed gone
/// (left or disconnected) and dropped. Their discovered terrain is kept.
const PLAYER_STALE_AFTER: Duration = Duration::from_secs(3);

/// One update streamed over the map track: either a player's live position or one
/// explored chunk's blocks. Both ends encode this with [`bincode`].
#[derive(Serialize, Deserialize)]
enum MapPacket {
    /// The sender's current avatar position (world pixels) and dimension.
    Pos { dim: Dimension, x: f32, y: f32 },
    /// One chunk the sender has loaded: its `CHUNK_AREA` block ids in row-major
    /// order, so the receiver can colour those cells on the shared map.
    Tile {
        dim: Dimension,
        cx: i32,
        cy: i32,
        blocks: Vec<BlockId>,
    },
}

/// A remote player's last-known position on the map.
#[derive(Clone, Copy)]
pub struct RemotePlayer {
    pub dim: Dimension,
    pub x: f32,
    pub y: f32,
    /// When this position last arrived, for staleness ([`PLAYER_STALE_AFTER`]).
    updated: Instant,
}

/// Everything received from other players, shared between the session thread and
/// the renderer.
#[derive(Default)]
pub struct MapShared {
    /// Latest position of every other player, keyed by entity id.
    players: HashMap<EntityId, RemotePlayer>,
    /// Explored chunks discovered by anyone, keyed by `(dimension, chunk coord)`.
    /// Newest report wins. Block ids are row-major, `CHUNK_AREA` long.
    tiles: HashMap<(Dimension, ChunkCoord), Vec<BlockId>>,
}

impl MapShared {
    /// Block ids of a remote-discovered chunk, or `None` if no player has shared
    /// it. Used by the map renderer for cells this client hasn't loaded itself.
    pub fn tile(&self, dim: Dimension, coord: ChunkCoord) -> Option<&[BlockId]> {
        self.tiles.get(&(dim, coord)).map(Vec::as_slice)
    }
}

/// The UI's handle to the map thread. Dropping it shuts the thread down (closing
/// the relay connection). Always receiving; the main loop feeds outbound updates.
pub struct MapHandle {
    /// Set on drop to stop the session thread.
    shutdown: Arc<AtomicBool>,
    /// Received positions and tiles, read by the renderer.
    shared: Arc<Mutex<MapShared>>,
    /// Outbound updates, drained by the publish task. Unbounded so the game loop
    /// never blocks on the network.
    out_tx: tokio::sync::mpsc::UnboundedSender<MapPacket>,
}

impl MapHandle {
    /// Report our avatar's current position and dimension to the other players.
    pub fn send_pos(&self, dim: Dimension, x: f32, y: f32) {
        let _ = self.out_tx.send(MapPacket::Pos { dim, x, y });
    }

    /// Share one explored chunk's blocks (`CHUNK_AREA` ids, row-major) so the
    /// other players' maps reveal it.
    pub fn send_tile(&self, dim: Dimension, coord: ChunkCoord, blocks: &[BlockId]) {
        let _ = self.out_tx.send(MapPacket::Tile {
            dim,
            cx: coord.0,
            cy: coord.1,
            blocks: blocks.to_vec(),
        });
    }

    /// Run `f` against the received map state under the lock. The renderer uses
    /// this to read many tiles without cloning each one.
    pub fn with_shared<R>(&self, f: impl FnOnce(&MapShared) -> R) -> R {
        f(&self.shared.lock())
    }

    /// Live remote players in `dim` (those whose last update is recent), as
    /// `(id, x, y)`. Stale entries are skipped; the renderer draws a marker for
    /// each. Cheap — only positions are copied.
    pub fn players_in(&self, dim: Dimension) -> Vec<(EntityId, f32, f32)> {
        self.shared
            .lock()
            .players
            .iter()
            .filter(|(_, p)| p.dim == dim && p.updated.elapsed() < PLAYER_STALE_AFTER)
            .map(|(&id, p)| (id, p.x, p.y))
            .collect()
    }
}

impl Drop for MapHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

/// Connect to the map relay at `relay_addr` (the same relay voice/webcam use),
/// pinning its certificate by `cert_hash`, and publish under `own_id`. Spawns the
/// session thread and returns immediately; failures surface as logs and an inert
/// handle that still accepts (and silently drops) outbound updates.
pub fn connect(relay_addr: SocketAddr, cert_hash: String, own_id: EntityId) -> MapHandle {
    let shutdown = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Mutex::new(MapShared::default()));
    let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel::<MapPacket>();

    let state = SessionState {
        relay_addr,
        cert_hash,
        own_id,
        shutdown: shutdown.clone(),
        shared: shared.clone(),
    };
    std::thread::Builder::new()
        .name("game-map".into())
        .spawn(move || session_thread(state, out_rx))
        .expect("spawn map session thread");

    MapHandle {
        shutdown,
        shared,
        out_tx,
    }
}

/// State moved into the session thread.
struct SessionState {
    relay_addr: SocketAddr,
    cert_hash: String,
    own_id: EntityId,
    shutdown: Arc<AtomicBool>,
    shared: Arc<Mutex<MapShared>>,
}

/// Body of the session thread: a current-thread tokio runtime driving connect +
/// publish + receive.
fn session_thread(state: SessionState, out_rx: tokio::sync::mpsc::UnboundedReceiver<MapPacket>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            log::warn!("map runtime unavailable: {e:#}");
            return;
        }
    };
    if let Err(e) = rt.block_on(session_main(state, out_rx)) {
        log::warn!("map session ended: {e:#}");
    }
}

/// Connect to the relay, publish our updates, and receive everyone else's until
/// shutdown or disconnect.
async fn session_main(
    state: SessionState,
    out_rx: tokio::sync::mpsc::UnboundedReceiver<MapPacket>,
) -> anyhow::Result<()> {
    // One origin for what we publish (our map), one for what we consume.
    let publish = Origin::random().produce();
    let consume = Origin::random().produce();

    // Our broadcast: a single map track. Subscribers address it by its
    // well-known name, exactly like voice and webcam do.
    let mut broadcast = moq_net::Broadcast::new().produce();
    let map_track = broadcast.create_track(Track {
        name: MAP_TRACK.to_string(),
        priority: 1,
    })?;
    publish.publish_broadcast(map_broadcast_path(state.own_id), broadcast.consume());

    // Connect to the relay, pinning its self-signed certificate by fingerprint.
    let mut config = moq_native::ClientConfig::default();
    config.bind = "0.0.0.0:0".parse().expect("valid bind");
    config.tls.fingerprint = vec![state.cert_hash.clone()];
    let client = config
        .init()?
        .with_publish(publish.consume())
        .with_consume(consume.clone());

    // WebTransport (https) over IP, certificate pinned by fingerprint — same as
    // voice/webcam (raw QUIC rejects the SNI-less IP path).
    let url = url::Url::parse(&format!("https://{}", state.relay_addr))?;
    let session = client.connect(url).await?;
    log::info!("map connected to relay at {}", state.relay_addr);

    let announced = consume.consume();

    tokio::select! {
        r = publish_loop(map_track, out_rx) => r?,
        r = receive_loop(announced, &state) => r?,
        _ = wait_shutdown(&state.shutdown) => {}
        _ = session.closed() => log::info!("map relay closed the connection"),
    }
    Ok(())
}

/// Poll the shutdown flag, returning once it is set.
async fn wait_shutdown(shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Publish each queued [`MapPacket`] as one [`hang`] frame in its own group. The
/// timestamp is a plain monotonic counter — the map doesn't sequence by time, but
/// `hang` frames carry one, so we just increment it.
async fn publish_loop(
    mut track: moq_net::TrackProducer,
    mut out_rx: tokio::sync::mpsc::UnboundedReceiver<MapPacket>,
) -> anyhow::Result<()> {
    let mut ts: u64 = 0;
    while let Some(packet) = out_rx.recv().await {
        let payload = match bincode::serialize(&packet) {
            Ok(bytes) => Bytes::from(bytes),
            Err(e) => {
                log::debug!("map encode failed: {e}");
                continue;
            }
        };
        let frame = hang::container::Frame {
            timestamp: hang::container::Timestamp::from_micros_unchecked(ts),
            payload,
        };
        let mut group = track.append_group()?;
        frame.encode(&mut group)?;
        group.finish()?;
        ts = ts.wrapping_add(1);
    }
    Ok(())
}

/// One remote player we're subscribed to: just its track reader (the map has no
/// per-player decoder state).
struct Remote {
    id: EntityId,
    track: moq_net::TrackConsumer,
}

/// Read the next frame from a remote, returning the (re-armed) remote and the
/// frame bytes (or `None` when its track ended).
async fn read_remote(mut remote: Remote) -> (Remote, Option<Bytes>) {
    let frame = remote.track.read_frame().await.ok().flatten();
    (remote, frame)
}

/// Discover other players' map broadcasts and fold their position/tile updates
/// into the shared state for the renderer.
async fn receive_loop(
    mut announced: moq_net::OriginConsumer,
    state: &SessionState,
) -> anyhow::Result<()> {
    let mut readers = FuturesUnordered::new();

    loop {
        tokio::select! {
            announce = announced.announced() => {
                let Some((path, broadcast)) = announce else { break };
                match broadcast {
                    Some(bc) => {
                        let Some(id) = map_entity_from_path(path.as_str()) else { continue };
                        if id == state.own_id {
                            continue; // never fold in our own map
                        }
                        let track = match bc.subscribe_track(&Track {
                            name: MAP_TRACK.to_string(),
                            priority: 1,
                        }) {
                            Ok(t) => t,
                            Err(e) => {
                                log::debug!("map subscribe failed for {id}: {e}");
                                continue;
                            }
                        };
                        readers.push(read_remote(Remote { id, track }));
                    }
                    None => {
                        // Broadcast unannounced (player left): drop their marker,
                        // but keep the terrain they revealed.
                        if let Some(id) = map_entity_from_path(path.as_str()) {
                            state.shared.lock().players.remove(&id);
                        }
                    }
                }
            }
            Some((remote, frame)) = readers.next() => {
                match frame {
                    Some(bytes) => {
                        apply_packet(remote.id, bytes, state);
                        readers.push(read_remote(remote));
                    }
                    None => {
                        // Track ended; drop the marker (terrain stays).
                        state.shared.lock().players.remove(&remote.id);
                    }
                }
            }
            else => break,
        }
    }
    Ok(())
}

/// Decode one map frame from `bytes` and apply it to the shared state.
fn apply_packet(id: EntityId, bytes: Bytes, state: &SessionState) {
    let frame = match hang::container::Frame::decode(bytes) {
        Ok(f) => f,
        Err(e) => {
            log::debug!("malformed map frame from {id}: {e}");
            return;
        }
    };
    let packet: MapPacket = match bincode::deserialize(&frame.payload) {
        Ok(p) => p,
        Err(e) => {
            log::debug!("undecodable map packet from {id}: {e}");
            return;
        }
    };
    let mut shared = state.shared.lock();
    match packet {
        MapPacket::Pos { dim, x, y } => {
            shared.players.insert(
                id,
                RemotePlayer {
                    dim,
                    x,
                    y,
                    updated: Instant::now(),
                },
            );
        }
        MapPacket::Tile {
            dim,
            cx,
            cy,
            blocks,
        } => {
            shared.tiles.insert((dim, (cx, cy)), blocks);
        }
    }
}
