//! Client-side webcam video: capture the camera, encode AV1, publish it over
//! [MOQ](https://moq.dev), and decode every other player's stream into RGBA
//! frames the renderer paints above their heads.
//!
//! A separate toggle from [`crate::client::voice`] but the same shape: a
//! background thread runs a tokio runtime driving the MOQ session, and the UI
//! talks to it through a cheap shared [`WebcamHandle`]. It rides the *same*
//! relay as voice (a different broadcast-path prefix keeps the two apart, see
//! [`crate::voice::video_broadcast_path`]).
//!
//! Pipeline:
//! - **Capture** (a dedicated blocking thread, only while transmitting): grab a
//!   camera frame, downscale to [`VIDEO_WIDTH`]×[`VIDEO_HEIGHT`], convert RGB→I420,
//!   AV1-encode it as an all-intra (every-frame-keyframe) packet, and hand it to
//!   the publish task. All-intra keeps each packet independently decodable, so a
//!   player who subscribes mid-stream sees video on the very next frame.
//! - **Publish**: wrap each AV1 packet in a [`hang`] frame on our single video
//!   track.
//! - **Subscribe/decode**: discover every other player's video broadcast, decode
//!   its AV1 packets to RGBA, and store the latest frame per player for the
//!   overlay to upload as an egui texture.
//!
//! Everything degrades gracefully: no camera just means we don't transmit (we
//! still receive), and the camera is never opened until the player toggles
//! transmission on (privacy). A failed relay connection logs and leaves the
//! handle inert.

use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::ptr::NonNull;
use std::slice;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::stream::FuturesUnordered;
use moq_net::bytes::Bytes;
use moq_net::{Origin, Track};
use parking_lot::Mutex;
use rav1e::prelude::*;
use yuvutils_rs::{
    YuvChromaSubsampling, YuvPlanarImage, YuvPlanarImageMut, YuvRange, YuvStandardMatrix,
    rgb_to_yuv420, yuv420_to_rgba,
};

use crate::entity::EntityId;
use crate::voice::{
    VIDEO_FPS, VIDEO_HEIGHT, VIDEO_TRACK, VIDEO_WIDTH, video_broadcast_path, video_entity_from_path,
};

// The colour space both ends agree on: 8-bit limited-range BT.601, the usual
// choice for small standard-definition video.
const YUV_RANGE: YuvRange = YuvRange::Limited;
const YUV_MATRIX: YuvStandardMatrix = YuvStandardMatrix::Bt601;

/// How long after the last decoded frame a remote stream is considered live. A
/// player who stops transmitting (presses `K` off, or disconnects) simply stops
/// sending frames — the relay keeps the broadcast announced — so the viewer drops
/// the thumbnail once nothing new has arrived for this long. Comfortably longer
/// than the inter-frame gap at [`VIDEO_FPS`], so normal streaming never blanks.
const STALE_AFTER: Duration = Duration::from_millis(500);

/// A decoded webcam frame: tightly-packed RGBA at its native size, plus a
/// monotonic sequence number so the overlay re-uploads its texture only when a
/// newer frame has arrived, and the time it decoded so stale streams can be
/// dropped.
#[derive(Clone)]
pub struct VideoFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    pub seq: u64,
    /// When this frame was decoded, for staleness ([`STALE_AFTER`]).
    pub updated: Instant,
}

/// The UI's handle to the webcam thread. Dropping it tells the thread to shut
/// down (closing the relay connection, releasing the camera, dropping decoders).
pub struct WebcamHandle {
    /// Whether our camera is transmitting. Toggled by the UI (`K`); the capture
    /// thread opens the camera while set and releases it while clear.
    capturing: Arc<AtomicBool>,
    /// Set on drop to stop the webcam threads.
    shutdown: Arc<AtomicBool>,
    /// Whether the camera is currently open and producing frames. Set by the
    /// capture thread; the UI uses it to report "no camera found".
    has_capture: Arc<AtomicBool>,
    /// Latest decoded frame from each remote player, keyed by entity id. Shared
    /// with the decode task; read by the overlay.
    frames: Arc<Mutex<HashMap<EntityId, VideoFrame>>>,
}

impl WebcamHandle {
    /// Start or stop transmitting our camera.
    pub fn set_capturing(&self, on: bool) {
        self.capturing.store(on, Ordering::Relaxed);
    }

    /// Whether our camera is currently transmitting.
    pub fn is_capturing(&self) -> bool {
        self.capturing.load(Ordering::Relaxed)
    }

    /// Whether a camera was found and is producing frames (only meaningful while
    /// [`Self::is_capturing`]).
    pub fn has_capture(&self) -> bool {
        self.has_capture.load(Ordering::Relaxed)
    }

    /// Cheap metadata for `id`'s latest frame: its sequence number and whether the
    /// stream is still live (a frame arrived within [`STALE_AFTER`]). `None` when
    /// we've never had a frame from this player. The overlay uses this every UI
    /// frame to decide whether to draw — and whether a re-upload is even needed —
    /// without cloning the pixels.
    pub fn frame_meta(&self, id: EntityId) -> Option<(u64, bool)> {
        let frames = self.frames.lock();
        let f = frames.get(&id)?;
        Some((f.seq, f.updated.elapsed() < STALE_AFTER))
    }

    /// The latest frame from `id`, but only if its sequence is newer than `since`
    /// (so the caller can skip re-uploading an unchanged texture). Returns the
    /// clone to upload, or `None` when there's nothing newer.
    pub fn frame_if_newer(&self, id: EntityId, since: u64) -> Option<VideoFrame> {
        let frames = self.frames.lock();
        let f = frames.get(&id)?;
        if f.seq > since { Some(f.clone()) } else { None }
    }
}

impl Drop for WebcamHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

/// Connect to the webcam relay at `relay_addr` (the same relay voice uses),
/// pinning its certificate by `cert_hash`, and publish under `own_id`. Spawns the
/// webcam threads and returns immediately; failures surface as logs and an inert
/// handle. Capture stays off until [`WebcamHandle::set_capturing`] is called.
pub fn connect(relay_addr: SocketAddr, cert_hash: String, own_id: EntityId) -> WebcamHandle {
    let capturing = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));
    let has_capture = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(Mutex::new(HashMap::new()));

    // Outbound AV1 packets: filled by the capture thread, drained by the publish
    // task. Unbounded so capture never blocks on the network.
    let (pkt_tx, pkt_rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();

    // Capture + encode run on their own blocking thread (the camera is blocking
    // and AV1 encode is heavy), pushing packets into the channel.
    {
        let capturing = capturing.clone();
        let shutdown = shutdown.clone();
        let has_capture = has_capture.clone();
        std::thread::Builder::new()
            .name("game-webcam-capture".into())
            .spawn(move || capture_thread(pkt_tx, capturing, shutdown, has_capture))
            .expect("spawn webcam capture thread");
    }

    // The session thread drives the async MOQ work (publish + subscribe/decode).
    let session = SessionState {
        relay_addr,
        cert_hash,
        own_id,
        shutdown: shutdown.clone(),
        frames: frames.clone(),
    };
    std::thread::Builder::new()
        .name("game-webcam".into())
        .spawn(move || session_thread(session, pkt_rx))
        .expect("spawn webcam session thread");

    WebcamHandle {
        capturing,
        shutdown,
        has_capture,
        frames,
    }
}

/// State moved into the session thread.
struct SessionState {
    relay_addr: SocketAddr,
    cert_hash: String,
    own_id: EntityId,
    shutdown: Arc<AtomicBool>,
    frames: Arc<Mutex<HashMap<EntityId, VideoFrame>>>,
}

/// Body of the session thread: a current-thread tokio runtime (so the `!Send`
/// AV1 decoders can live on it) driving connect + publish + decode.
fn session_thread(state: SessionState, pkt_rx: tokio::sync::mpsc::UnboundedReceiver<Bytes>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            log::warn!("webcam runtime unavailable: {e:#}");
            return;
        }
    };
    if let Err(e) = rt.block_on(session_main(state, pkt_rx)) {
        log::warn!("webcam session ended: {e:#}");
    }
}

/// Connect to the relay, publish our camera, and decode everyone else's until
/// shutdown or disconnect.
async fn session_main(
    state: SessionState,
    pkt_rx: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
) -> anyhow::Result<()> {
    // One origin for what we publish (our camera), one for what we consume.
    let publish = Origin::random().produce();
    let consume = Origin::random().produce();

    // Our broadcast: a single AV1 video track. No catalog is needed — subscribers
    // address the track by its well-known name, exactly like voice does.
    let mut broadcast = moq_net::Broadcast::new().produce();
    let video_track = broadcast.create_track(Track {
        name: VIDEO_TRACK.to_string(),
        priority: 1,
    })?;
    publish.publish_broadcast(video_broadcast_path(state.own_id), broadcast.consume());

    // Connect to the relay, pinning its self-signed certificate by fingerprint.
    let mut config = moq_native::ClientConfig::default();
    config.bind = "0.0.0.0:0".parse().expect("valid bind");
    config.tls.fingerprint = vec![state.cert_hash.clone()];
    let client = config
        .init()?
        .with_publish(publish.consume())
        .with_consume(consume.clone());

    // WebTransport (https), not raw QUIC (moql): the relay is addressed by IP and
    // TLS omits SNI for IPs, which moq-native's raw-QUIC path rejects. The
    // certificate is pinned by fingerprint regardless of host. (Same as voice.)
    let url = url::Url::parse(&format!("https://{}", state.relay_addr))?;
    let session = client.connect(url).await?;
    log::info!("webcam connected to relay at {}", state.relay_addr);

    let announced = consume.consume();

    tokio::select! {
        r = publish_loop(video_track, pkt_rx) => r?,
        r = decode_loop(announced, &state) => r?,
        _ = wait_shutdown(&state.shutdown) => {}
        _ = session.closed() => log::info!("webcam relay closed the connection"),
    }
    Ok(())
}

/// Poll the shutdown flag, returning once it is set.
async fn wait_shutdown(shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Publish each captured AV1 packet as one [`hang`] frame in its own group, with
/// a monotonically increasing presentation timestamp.
async fn publish_loop(
    mut track: moq_net::TrackProducer,
    mut pkt_rx: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
) -> anyhow::Result<()> {
    // Microseconds per frame at the target rate, the increment for timestamps.
    let frame_micros = 1_000_000 / VIDEO_FPS as u64;
    let mut ts: u64 = 0;
    while let Some(packet) = pkt_rx.recv().await {
        let frame = hang::container::Frame {
            timestamp: hang::container::Timestamp::from_micros_unchecked(ts),
            payload: packet,
        };
        let mut group = track.append_group()?;
        frame.encode(&mut group)?;
        group.finish()?;
        ts = ts.wrapping_add(frame_micros);
    }
    Ok(())
}

/// One remote player we're subscribed to: its track reader and AV1 decoder.
struct Remote {
    id: EntityId,
    track: moq_net::TrackConsumer,
    decoder: Av1Decoder,
}

/// Read the next frame from a remote, returning the (re-armed) remote and the
/// frame bytes (or `None` when its track ended).
async fn read_remote(mut remote: Remote) -> (Remote, Option<Bytes>) {
    let frame = remote.track.read_frame().await.ok().flatten();
    (remote, frame)
}

/// Discover other players' webcam broadcasts and decode their video into the
/// shared frame map for the overlay to render.
async fn decode_loop(
    mut announced: moq_net::OriginConsumer,
    state: &SessionState,
) -> anyhow::Result<()> {
    let mut seq: u64 = 0;
    let mut readers = FuturesUnordered::new();

    loop {
        tokio::select! {
            announce = announced.announced() => {
                let Some((path, broadcast)) = announce else { break };
                match broadcast {
                    Some(bc) => {
                        let Some(id) = video_entity_from_path(path.as_str()) else { continue };
                        if id == state.own_id {
                            continue; // never decode our own video
                        }
                        let track = match bc.subscribe_track(&Track {
                            name: VIDEO_TRACK.to_string(),
                            priority: 1,
                        }) {
                            Ok(t) => t,
                            Err(e) => {
                                log::debug!("webcam subscribe failed for {id}: {e}");
                                continue;
                            }
                        };
                        let decoder = match Av1Decoder::new() {
                            Some(d) => d,
                            None => {
                                log::warn!("AV1 decoder init failed; skipping {id}");
                                continue;
                            }
                        };
                        readers.push(read_remote(Remote { id, track, decoder }));
                    }
                    None => {
                        // Broadcast unannounced (player left): forget their frame.
                        if let Some(id) = video_entity_from_path(path.as_str()) {
                            state.frames.lock().remove(&id);
                        }
                    }
                }
            }
            Some((mut remote, frame)) = readers.next() => {
                match frame {
                    Some(bytes) => {
                        decode_frame(&mut remote, bytes, state, &mut seq);
                        readers.push(read_remote(remote));
                    }
                    None => {
                        // Track ended; drop the remote (and its decoder).
                        state.frames.lock().remove(&remote.id);
                    }
                }
            }
            else => break,
        }
    }
    Ok(())
}

/// Decode one AV1 frame from `bytes` and store the resulting RGBA image as the
/// remote's latest frame.
fn decode_frame(remote: &mut Remote, bytes: Bytes, state: &SessionState, seq: &mut u64) {
    let frame = match hang::container::Frame::decode(bytes) {
        Ok(f) => f,
        Err(e) => {
            log::debug!("malformed webcam frame from {}: {e}", remote.id);
            return;
        }
    };
    if let Some((w, h, rgba)) = remote.decoder.decode(&frame.payload) {
        *seq = seq.wrapping_add(1);
        state.frames.lock().insert(
            remote.id,
            VideoFrame {
                width: w,
                height: h,
                rgba,
                seq: *seq,
                updated: Instant::now(),
            },
        );
    }
}

/// The blocking capture+encode loop. Idle (camera closed) until transmission is
/// toggled on; then opens the camera, encodes frames at [`VIDEO_FPS`], and
/// releases the camera again when toggled off. Exits on shutdown.
fn capture_thread(
    pkt_tx: tokio::sync::mpsc::UnboundedSender<Bytes>,
    capturing: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    has_capture: Arc<AtomicBool>,
) {
    let frame_interval = Duration::from_micros(1_000_000 / VIDEO_FPS as u64);

    while !shutdown.load(Ordering::Relaxed) {
        if !capturing.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        // Open the camera only now that we're transmitting (so its light stays
        // off otherwise). On failure, back off and retry while still toggled on.
        let mut camera = match open_camera() {
            Some(c) => c,
            None => {
                has_capture.store(false, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        let mut encoder = match build_encoder() {
            Some(e) => e,
            None => {
                log::warn!("AV1 encoder init failed; webcam transmit disabled");
                has_capture.store(false, Ordering::Relaxed);
                capturing.store(false, Ordering::Relaxed);
                continue;
            }
        };
        has_capture.store(true, Ordering::Relaxed);
        log::info!("webcam capture active");

        while capturing.load(Ordering::Relaxed) && !shutdown.load(Ordering::Relaxed) {
            let started = Instant::now();
            if let Some(i420) = grab_i420(&mut camera) {
                if let Some(packet) = encode_frame(&mut encoder, &i420) {
                    // The receiver is dropped only at shutdown; ignore send errors.
                    let _ = pkt_tx.send(Bytes::from(packet));
                }
            }
            if let Some(remaining) = frame_interval.checked_sub(started.elapsed()) {
                std::thread::sleep(remaining);
            }
        }

        // Toggled off (or shutting down): release the camera so its light goes out.
        drop(camera);
        has_capture.store(false, Ordering::Relaxed);
        log::info!("webcam capture stopped");
    }
}

/// Open the default camera at whatever its highest resolution is (we downscale to
/// a thumbnail regardless), starting its stream. `None` if no camera is usable.
fn open_camera() -> Option<nokhwa::Camera> {
    use nokhwa::pixel_format::RgbFormat;
    use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};

    let requested =
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestResolution);
    let mut camera = match nokhwa::Camera::new(CameraIndex::Index(0), requested) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("could not open camera, webcam transmit disabled: {e}");
            return None;
        }
    };
    if let Err(e) = camera.open_stream() {
        log::warn!("could not start camera stream: {e}");
        return None;
    }
    Some(camera)
}

/// Tightly-packed I420 planes at [`VIDEO_WIDTH`]×[`VIDEO_HEIGHT`], owned so the
/// encoder can read them after the camera buffer is gone.
struct I420 {
    yuv: YuvPlanarImageMut<'static, u8>,
}

/// Grab one camera frame, downscale it to the target thumbnail size, and convert
/// it to I420. `None` if the camera frame couldn't be read or decoded.
fn grab_i420(camera: &mut nokhwa::Camera) -> Option<I420> {
    use nokhwa::pixel_format::RgbFormat;

    let buffer = camera.frame().ok()?;
    let rgb = buffer.decode_image::<RgbFormat>().ok()?;
    // Downscale to the thumbnail size. `image` is already a dependency.
    let small = image::imageops::resize(
        &rgb,
        VIDEO_WIDTH as u32,
        VIDEO_HEIGHT as u32,
        image::imageops::FilterType::Triangle,
    );
    let rgb_data = small.into_raw(); // VIDEO_WIDTH*VIDEO_HEIGHT*3 bytes

    let mut yuv = YuvPlanarImageMut::<u8>::alloc(
        VIDEO_WIDTH as u32,
        VIDEO_HEIGHT as u32,
        YuvChromaSubsampling::Yuv420,
    );
    rgb_to_yuv420(
        &mut yuv,
        &rgb_data,
        VIDEO_WIDTH as u32 * 3,
        YUV_RANGE,
        YUV_MATRIX,
    )
    .ok()?;
    Some(I420 { yuv })
}

/// Build an AV1 encoder configured for tiny, low-latency, all-intra video.
fn build_encoder() -> Option<Context<u8>> {
    let mut enc = EncoderConfig::with_speed_preset(10); // 10 = fastest
    enc.width = VIDEO_WIDTH;
    enc.height = VIDEO_HEIGHT;
    enc.bit_depth = 8;
    enc.chroma_sampling = ChromaSampling::Cs420;
    enc.low_latency = true;
    enc.speed_settings.rdo_lookahead_frames = 1;

    let cfg = Config::new().with_encoder_config(enc).with_threads(1);
    cfg.new_context::<u8>().ok()
}

/// Encode one I420 frame as a self-contained (forced-keyframe) AV1 packet.
fn encode_frame(ctx: &mut Context<u8>, frame: &I420) -> Option<Vec<u8>> {
    let src = frame.yuv.to_fixed();
    let mut f = ctx.new_frame();
    f.planes[0].copy_from_raw_u8(src.y_plane, src.y_stride as usize, 1);
    f.planes[1].copy_from_raw_u8(src.u_plane, src.u_stride as usize, 1);
    f.planes[2].copy_from_raw_u8(src.v_plane, src.v_stride as usize, 1);

    // Force every frame to be a keyframe so each packet decodes on its own — late
    // subscribers then see video immediately instead of waiting for a GOP start.
    let params = FrameParameters {
        frame_type_override: FrameTypeOverride::Key,
        opaque: None,
        t35_metadata: Box::new([]),
    };
    if ctx.send_frame((f, params)).is_err() {
        return None;
    }
    loop {
        match ctx.receive_packet() {
            Ok(packet) => return Some(packet.data),
            Err(EncoderStatus::Encoded) => continue,
            Err(_) => return None,
        }
    }
}

/// A safe-ish wrapper over rav1d's `dav1d` C ABI (the only API the crate
/// exposes). All calls are pure Rust — nothing links an external C library — but
/// they are `unsafe`, so the unsafety is contained here.
struct Av1Decoder {
    ctx: Option<rav1d::include::dav1d::dav1d::Dav1dContext>,
}

impl Av1Decoder {
    /// Open a single-threaded AV1 decoder, or `None` on failure.
    fn new() -> Option<Self> {
        use rav1d::include::dav1d::dav1d::Dav1dSettings;
        use rav1d::src::lib::{dav1d_default_settings, dav1d_open};

        // SAFETY: `dav1d_default_settings` fully initialises the struct we pass a
        // pointer to; we then read it back as initialised.
        let mut settings = unsafe {
            let mut s = MaybeUninit::<Dav1dSettings>::uninit();
            dav1d_default_settings(NonNull::new(s.as_mut_ptr()).unwrap());
            s.assume_init()
        };
        settings.n_threads = 1;

        let mut ctx: Option<rav1d::include::dav1d::dav1d::Dav1dContext> = None;
        // SAFETY: both pointers are to valid, writable locals; `settings` was
        // produced by `dav1d_default_settings`.
        let res = unsafe { dav1d_open(NonNull::new(&mut ctx), NonNull::new(&mut settings)) };
        if res.0 != 0 || ctx.is_none() {
            return None;
        }
        Some(Av1Decoder { ctx })
    }

    /// Decode one AV1 packet, returning the latest available picture as
    /// `(width, height, rgba)`, or `None` if no full picture is ready yet.
    fn decode(&mut self, packet: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
        use rav1d::include::dav1d::data::Dav1dData;
        use rav1d::src::lib::{dav1d_data_create, dav1d_data_unref, dav1d_send_data};

        if packet.is_empty() {
            return None;
        }

        // Hand the packet to the decoder via a dav1d-owned buffer (it frees it
        // once consumed, so there's no free-callback to manage).
        let mut data = Dav1dData::default();
        // SAFETY: `data` is a valid local; `dav1d_data_create` allocates `len`
        // bytes inside it and returns a pointer to copy into.
        let dst = unsafe { dav1d_data_create(NonNull::new(&mut data), packet.len()) };
        if dst.is_null() {
            return None;
        }
        // SAFETY: `dst` points to exactly `packet.len()` freshly allocated bytes.
        unsafe { std::ptr::copy_nonoverlapping(packet.as_ptr(), dst, packet.len()) };

        let mut latest = None;
        loop {
            // SAFETY: `self.ctx` is a live context from `dav1d_open`; `data` is a
            // valid, decoder-owned buffer.
            let res = unsafe { dav1d_send_data(self.ctx, NonNull::new(&mut data)) };
            if res.0 == 0 {
                break; // packet consumed
            }
            if res.0 == -libc::EAGAIN {
                // Decoder is full: drain a picture, then retry the same data.
                if let Some(f) = self.get_picture() {
                    latest = Some(f);
                }
                continue;
            }
            // Real error: release the unconsumed buffer and stop.
            if data.sz != 0 {
                // SAFETY: `data` still owns its buffer; unref frees it.
                unsafe { dav1d_data_unref(NonNull::new(&mut data)) };
            }
            break;
        }

        // Drain any pictures now available, keeping the most recent.
        while let Some(f) = self.get_picture() {
            latest = Some(f);
        }
        latest
    }

    /// Pull one decoded picture, if any, converting it to RGBA.
    fn get_picture(&mut self) -> Option<(usize, usize, Vec<u8>)> {
        use rav1d::include::dav1d::picture::Dav1dPicture;
        use rav1d::src::lib::{dav1d_get_picture, dav1d_picture_unref};

        let mut pic = Dav1dPicture::default();
        // SAFETY: `self.ctx` is live; `pic` is a valid, writable local.
        let res = unsafe { dav1d_get_picture(self.ctx, NonNull::new(&mut pic)) };
        if res.0 != 0 {
            return None;
        }
        // Extract before unref (the planes belong to the picture).
        let out = picture_to_rgba(&pic);
        // SAFETY: `pic` was filled by `dav1d_get_picture`; unref releases it.
        unsafe { dav1d_picture_unref(NonNull::new(&mut pic)) };
        out
    }
}

impl Drop for Av1Decoder {
    fn drop(&mut self) {
        use rav1d::src::lib::dav1d_close;
        if self.ctx.is_some() {
            // SAFETY: `self.ctx` is a live context from `dav1d_open`, not yet
            // closed; `dav1d_close` takes ownership and clears the Option.
            unsafe { dav1d_close(NonNull::new(&mut self.ctx)) };
        }
    }
}

/// Convert a decoded 8-bit I420 [`Dav1dPicture`] into tightly-packed RGBA.
/// `None` for any non-8-bit picture (we only ever encode 8-bit 4:2:0).
fn picture_to_rgba(
    pic: &rav1d::include::dav1d::picture::Dav1dPicture,
) -> Option<(usize, usize, Vec<u8>)> {
    if pic.p.bpc != 8 {
        return None;
    }
    let w = pic.p.w as usize;
    let h = pic.p.h as usize;
    if w == 0 || h == 0 {
        return None;
    }
    let y_ptr = pic.data[0]?.as_ptr() as *const u8;
    let u_ptr = pic.data[1]?.as_ptr() as *const u8;
    let v_ptr = pic.data[2]?.as_ptr() as *const u8;
    let y_stride = pic.stride[0] as usize;
    let c_stride = pic.stride[1] as usize;
    let ch = h.div_ceil(2);

    // SAFETY: dav1d guarantees each plane spans `stride * height` bytes (chroma at
    // half height), and the picture outlives this borrow (we unref after).
    let (y_plane, u_plane, v_plane) = unsafe {
        (
            slice::from_raw_parts(y_ptr, y_stride * h),
            slice::from_raw_parts(u_ptr, c_stride * ch),
            slice::from_raw_parts(v_ptr, c_stride * ch),
        )
    };

    let src = YuvPlanarImage {
        y_plane,
        y_stride: y_stride as u32,
        u_plane,
        u_stride: c_stride as u32,
        v_plane,
        v_stride: c_stride as u32,
        width: w as u32,
        height: h as u32,
    };
    let mut rgba = vec![0u8; w * h * 4];
    yuv420_to_rgba(&src, &mut rgba, (w * 4) as u32, YUV_RANGE, YUV_MATRIX).ok()?;
    Some((w, h, rgba))
}
