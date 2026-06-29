//! Client-side voice chat: capture the microphone, encode Opus, publish it over
//! [MOQ](https://moq.dev), and play back every other player's stream.
//!
//! Mirrors the structure of [`crate::client::net`]: a background thread runs a
//! tokio runtime driving the MOQ session, and the UI talks to it through cheap
//! shared handles ([`VoiceHandle`]). The relay is the game server's in-process
//! [`crate::voice_relay`]; audio is global (everyone hears everyone).
//!
//! Pipeline:
//! - **Capture** (a `cpal` input stream): downmix to mono, resample to 48 kHz,
//!   accumulate 20 ms frames, and — only while push-to-talk is held — Opus-encode
//!   each frame and hand it to the publish task.
//! - **Publish**: wrap each Opus packet in a [`hang`] frame and append it to our
//!   broadcast's single audio track.
//! - **Subscribe**: discover every other player's broadcast, read its Opus
//!   frames, decode them, and feed each speaker its own `rodio` sink (rodio mixes
//!   the sinks for us).
//!
//! Everything degrades gracefully: no microphone just means we don't transmit
//! (we still hear others), and no audio output means we don't play (we can still
//! transmit). A failed relay connection logs and leaves the handle inert.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use audiopus::coder::{Decoder, Encoder};
use audiopus::{Application, Channels, SampleRate};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use moq_net::bytes::Bytes;
use moq_net::{Origin, Track};
use parking_lot::Mutex;
use rodio::buffer::SamplesBuffer;
use rodio::{OutputStream, Sink};

use crate::entity::EntityId;
use crate::voice::{
    AUDIO_TRACK, CHANNELS, FRAME_MICROS, FRAME_SAMPLES, SAMPLE_RATE, broadcast_path,
    entity_from_path,
};

/// A speaker is shown as "talking" in the HUD until this long after their last
/// decoded frame.
const SPEAKING_TIMEOUT: Duration = Duration::from_millis(400);

/// The UI's handle to the voice thread. Dropping it tells the thread to shut down
/// (closing the relay connection and stopping all audio).
pub struct VoiceHandle {
    /// Push-to-talk: while `true` (and [`Self::enabled`]), captured audio is
    /// transmitted. Toggled by the UI as the talk key is pressed/released.
    ptt: Arc<AtomicBool>,
    /// Local voice on/off. When `false`, we neither transmit nor play back, so a
    /// player can silence voice entirely without leaving the server.
    enabled: Arc<AtomicBool>,
    /// Set on drop to stop the voice thread.
    shutdown: Arc<AtomicBool>,
    /// Last time a frame was decoded from each remote speaker, used to drive the
    /// "who is talking" HUD. Shared with the voice thread.
    speaking: Arc<Mutex<HashMap<EntityId, Instant>>>,
    /// Whether a usable microphone was found. Set by the voice thread once it has
    /// opened (or failed to open) the capture device, since the `cpal` stream is
    /// `!Send` and must be built there.
    has_input: Arc<AtomicBool>,
}

impl VoiceHandle {
    /// Set the push-to-talk state (true while the talk key is held).
    pub fn set_ptt(&self, on: bool) {
        self.ptt.store(on, Ordering::Relaxed);
    }

    /// Turn voice on or off locally (mutes both transmit and playback).
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// Whether voice is currently enabled locally.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Whether a microphone was available to capture from.
    pub fn has_input(&self) -> bool {
        self.has_input.load(Ordering::Relaxed)
    }

    /// Whether we are transmitting right now (voice enabled, mic present, and
    /// push-to-talk held), for the HUD's own "talking" indicator.
    pub fn transmitting(&self) -> bool {
        self.has_input.load(Ordering::Relaxed)
            && self.enabled.load(Ordering::Relaxed)
            && self.ptt.load(Ordering::Relaxed)
    }

    /// Entity ids of remote players heard within [`SPEAKING_TIMEOUT`], for the HUD.
    pub fn talking(&self) -> Vec<EntityId> {
        let now = Instant::now();
        self.speaking
            .lock()
            .iter()
            .filter(|(_, t)| now.duration_since(**t) < SPEAKING_TIMEOUT)
            .map(|(id, _)| *id)
            .collect()
    }
}

impl Drop for VoiceHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

/// Connect to the voice relay at `relay_addr` (the game server's host plus the
/// voice port), pinning its certificate by `cert_hash`, and publish under
/// `own_id`. Spawns the voice thread and returns immediately; failures surface as
/// logs and an inert handle.
pub fn connect(relay_addr: SocketAddr, cert_hash: String, own_id: EntityId) -> VoiceHandle {
    let ptt = Arc::new(AtomicBool::new(false));
    let enabled = Arc::new(AtomicBool::new(true));
    let shutdown = Arc::new(AtomicBool::new(false));
    let speaking = Arc::new(Mutex::new(HashMap::new()));
    let has_input = Arc::new(AtomicBool::new(false));

    // Outbound Opus packets: filled by the cpal capture callback, drained by the
    // publish task. Unbounded so the audio callback never blocks.
    let (pkt_tx, pkt_rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();

    let thread_state = ThreadState {
        relay_addr,
        cert_hash,
        own_id,
        ptt: ptt.clone(),
        enabled: enabled.clone(),
        shutdown: shutdown.clone(),
        speaking: speaking.clone(),
        has_input: has_input.clone(),
        pkt_tx,
    };

    std::thread::Builder::new()
        .name("game-voice".into())
        .spawn(move || run_thread(thread_state, pkt_rx))
        .expect("spawn voice thread");

    VoiceHandle {
        ptt,
        enabled,
        shutdown,
        speaking,
        has_input,
    }
}

/// State moved into the voice thread.
struct ThreadState {
    relay_addr: SocketAddr,
    cert_hash: String,
    own_id: EntityId,
    ptt: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    speaking: Arc<Mutex<HashMap<EntityId, Instant>>>,
    has_input: Arc<AtomicBool>,
    /// Sender the capture stream pushes encoded Opus packets into.
    pkt_tx: tokio::sync::mpsc::UnboundedSender<Bytes>,
}

/// Body of the voice thread: build the (`!Send`) capture stream here, keep it
/// alive, and drive the async session on a current-thread runtime (so the
/// `!Send` decoders and rodio sinks can stay on this thread too).
fn run_thread(state: ThreadState, pkt_rx: tokio::sync::mpsc::UnboundedReceiver<Bytes>) {
    // Build microphone capture on this thread (cpal streams are not `Send`), and
    // record whether one is available for the UI. Held for the thread's lifetime;
    // dropping it stops capture.
    let _capture = build_capture(
        state.pkt_tx.clone(),
        state.ptt.clone(),
        state.enabled.clone(),
    );
    state.has_input.store(_capture.is_some(), Ordering::Relaxed);

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            log::warn!("voice runtime unavailable: {e:#}");
            return;
        }
    };

    if let Err(e) = rt.block_on(session_main(state, pkt_rx)) {
        log::warn!("voice session ended: {e:#}");
    }
}

/// The async heart of the voice client: connect, publish our mic, and play back
/// everyone else until shutdown or disconnect.
async fn session_main(
    state: ThreadState,
    pkt_rx: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
) -> anyhow::Result<()> {
    // One origin for what we publish (our mic), one for what we consume (others).
    let publish = Origin::random().produce();
    let consume = Origin::random().produce();

    // Our broadcast: a catalog plus the single Opus audio track.
    let mut broadcast = moq_net::Broadcast::new().produce();
    let _catalog = publish_catalog(&mut broadcast)?;
    let audio_track = broadcast.create_track(Track {
        name: AUDIO_TRACK.to_string(),
        priority: 1,
    })?;
    publish.publish_broadcast(broadcast_path(state.own_id), broadcast.consume());

    // Connect to the relay, pinning its self-signed certificate by fingerprint.
    let mut config = moq_native::ClientConfig::default();
    config.bind = "0.0.0.0:0".parse().expect("valid bind");
    config.tls.fingerprint = vec![state.cert_hash.clone()];
    let client = config
        .init()?
        .with_publish(publish.consume())
        .with_consume(consume.clone());

    let url = format!("moql://{}", state.relay_addr);
    let url = url::Url::parse(&url)?;
    let session = client.connect(url).await?;
    log::info!("voice connected to relay at {}", state.relay_addr);

    let announced = consume.consume();

    tokio::select! {
        r = publish_loop(audio_track, pkt_rx) => r?,
        r = playback_loop(announced, &state) => r?,
        _ = wait_shutdown(&state.shutdown) => {}
        _ = session.closed() => log::info!("voice relay closed the connection"),
    }
    Ok(())
}

/// Poll the shutdown flag, returning once it is set.
async fn wait_shutdown(shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Publish each captured Opus packet as one [`hang`] frame in its own group, with
/// a monotonically increasing presentation timestamp.
async fn publish_loop(
    mut track: moq_net::TrackProducer,
    mut pkt_rx: tokio::sync::mpsc::UnboundedReceiver<Bytes>,
) -> anyhow::Result<()> {
    let mut ts: u64 = 0;
    while let Some(packet) = pkt_rx.recv().await {
        let frame = hang::container::Frame {
            timestamp: hang::container::Timestamp::from_micros_unchecked(ts),
            payload: packet,
        };
        let mut group = track.append_group()?;
        frame.encode(&mut group)?;
        group.finish()?;
        ts = ts.wrapping_add(FRAME_MICROS);
    }
    Ok(())
}

/// One remote speaker we're subscribed to: its track reader, Opus decoder, and
/// the rodio sink it plays through.
struct Remote {
    id: EntityId,
    track: moq_net::TrackConsumer,
    decoder: Decoder,
    sink: Option<Sink>,
}

/// Read the next frame from a remote, returning the (re-armed) remote and the
/// frame bytes (or `None` when its track ended).
async fn read_remote(mut remote: Remote) -> (Remote, Option<Bytes>) {
    let frame = remote.track.read_frame().await.ok().flatten();
    (remote, frame)
}

/// Discover other players' broadcasts and play their audio. Opens its own rodio
/// output device; if none is available, frames are still drained (so the relay
/// doesn't back up) but produce no sound.
async fn playback_loop(
    mut announced: moq_net::OriginConsumer,
    state: &ThreadState,
) -> anyhow::Result<()> {
    let output = match OutputStream::try_default() {
        Ok((stream, handle)) => Some((stream, handle)),
        Err(e) => {
            log::warn!("voice playback unavailable: {e:#}");
            None
        }
    };
    // The stream guard must stay alive for the handle to keep playing.
    let handle = output.as_ref().map(|(_, h)| h.clone());

    let mut pcm = vec![0i16; (SAMPLE_RATE as usize / 1000) * 120]; // up to 120 ms
    let mut readers = FuturesUnordered::new();

    loop {
        tokio::select! {
            announce = announced.announced() => {
                let Some((path, broadcast)) = announce else { break };
                match broadcast {
                    Some(bc) => {
                        let Some(id) = entity_from_path(path.as_str()) else { continue };
                        if id == state.own_id {
                            continue; // never play our own voice back
                        }
                        let track = match bc.subscribe_track(&Track {
                            name: AUDIO_TRACK.to_string(),
                            priority: 1,
                        }) {
                            Ok(t) => t,
                            Err(e) => {
                                log::debug!("voice subscribe failed for {id}: {e}");
                                continue;
                            }
                        };
                        let decoder = match Decoder::new(SampleRate::Hz48000, Channels::Mono) {
                            Ok(d) => d,
                            Err(e) => {
                                log::warn!("opus decoder init failed: {e}");
                                continue;
                            }
                        };
                        let sink = handle.as_ref().and_then(|h| Sink::try_new(h).ok());
                        readers.push(read_remote(Remote { id, track, decoder, sink }));
                    }
                    None => {
                        // Broadcast unannounced (player left): forget their speaking state.
                        if let Some(id) = entity_from_path(path.as_str()) {
                            state.speaking.lock().remove(&id);
                        }
                    }
                }
            }
            Some((mut remote, frame)) = readers.next() => {
                match frame {
                    Some(bytes) => {
                        play_frame(&mut remote, bytes, &mut pcm, state);
                        readers.push(read_remote(remote));
                    }
                    None => {
                        // Track ended; drop the remote and its sink.
                        state.speaking.lock().remove(&remote.id);
                    }
                }
            }
            else => break,
        }
    }
    Ok(())
}

/// Decode one Opus frame from `bytes` and queue it on the remote's sink, marking
/// the speaker as currently talking. A no-op for output when voice is muted
/// locally or no audio device exists, but the frame is still consumed.
fn play_frame(remote: &mut Remote, bytes: Bytes, pcm: &mut [i16], state: &ThreadState) {
    let frame = match hang::container::Frame::decode(bytes) {
        Ok(f) => f,
        Err(e) => {
            log::debug!("malformed voice frame from {}: {e}", remote.id);
            return;
        }
    };

    if !state.enabled.load(Ordering::Relaxed) {
        return;
    }

    // audiopus 0.3 wants typed `Packet` / `MutSignals` wrappers (built via
    // `TryFrom`); an empty payload can't form a packet, so skip it.
    let Ok(packet) = audiopus::packet::Packet::try_from(&frame.payload[..]) else {
        return;
    };
    let Ok(out) = audiopus::MutSignals::try_from(&mut pcm[..]) else {
        return;
    };
    let samples = match remote.decoder.decode(Some(packet), out, false) {
        Ok(n) => n,
        Err(e) => {
            log::debug!("opus decode failed from {}: {e}", remote.id);
            return;
        }
    };

    state.speaking.lock().insert(remote.id, Instant::now());

    if let Some(sink) = &remote.sink {
        sink.append(SamplesBuffer::new(
            CHANNELS,
            SAMPLE_RATE,
            pcm[..samples].to_vec(),
        ));
    }
}

/// Create the broadcast's catalog track and write a single catalog describing our
/// one Opus/48 kHz/mono audio rendition. Returns the producer, which must be kept
/// alive for the catalog track to stay published.
fn publish_catalog(
    broadcast: &mut moq_net::BroadcastProducer,
) -> anyhow::Result<moq_net::TrackProducer> {
    let mut catalog = hang::Catalog::default();
    catalog.audio.insert(
        AUDIO_TRACK,
        hang::catalog::AudioConfig::new(
            hang::catalog::AudioCodec::Opus,
            SAMPLE_RATE,
            CHANNELS as u32,
        ),
    )?;
    let json = catalog.to_vec()?;

    let mut track = broadcast.create_track(hang::Catalog::default_track())?;
    track.write_frame(Bytes::from(json))?;
    Ok(track)
}

/// Build the microphone capture stream, encoding 20 ms Opus frames into `pkt_tx`
/// while `ptt` and `enabled` are both set. Returns `None` (with a warning) when no
/// input device is available, so the rest of voice still works.
fn build_capture(
    pkt_tx: tokio::sync::mpsc::UnboundedSender<Bytes>,
    ptt: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
) -> Option<cpal::Stream> {
    let host = cpal::default_host();
    let device = host.default_input_device()?;
    let supported = device.default_input_config().ok()?;

    let in_rate = supported.sample_rate().0 as f64;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let encoder = match Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("opus encoder init failed, microphone disabled: {e}");
            return None;
        }
    };

    let mut capture = Capture {
        in_rate,
        pos: 0.0,
        inbuf: Vec::new(),
        frame: Vec::with_capacity(FRAME_SAMPLES * 2),
        encoder,
        out: vec![0u8; 4000],
        ptt,
        enabled,
        tx: pkt_tx,
    };

    let err = |e| log::warn!("voice input stream error: {e}");

    let stream = match format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &_| {
                for frame in data.chunks(channels) {
                    let m = frame.iter().copied().sum::<f32>() / channels as f32;
                    capture.inbuf.push(m);
                }
                capture.process();
            },
            err,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _: &_| {
                for frame in data.chunks(channels) {
                    let m =
                        frame.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / channels as f32;
                    capture.inbuf.push(m);
                }
                capture.process();
            },
            err,
            None,
        ),
        other => {
            log::warn!("unsupported microphone sample format {other:?}; voice input disabled");
            return None;
        }
    };

    match stream {
        Ok(stream) => {
            if let Err(e) = stream.play() {
                log::warn!("could not start microphone: {e}");
                return None;
            }
            log::info!("voice microphone active ({in_rate} Hz, {channels} ch)");
            Some(stream)
        }
        Err(e) => {
            log::warn!("could not open microphone, voice input disabled: {e}");
            None
        }
    }
}

/// Microphone capture state: resamples incoming mono audio to 48 kHz and packs it
/// into 20 ms Opus frames. Lives inside the cpal callback closure.
struct Capture {
    /// The device's input sample rate (Hz).
    in_rate: f64,
    /// Fractional read cursor into `inbuf`, in input samples.
    pos: f64,
    /// Accumulated mono input samples awaiting resampling.
    inbuf: Vec<f32>,
    /// Resampled 48 kHz mono samples awaiting packetisation into Opus frames.
    frame: Vec<i16>,
    encoder: Encoder,
    /// Scratch buffer for one encoded Opus packet.
    out: Vec<u8>,
    ptt: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<Bytes>,
}

impl Capture {
    /// Resample whatever is buffered to 48 kHz, emitting Opus frames as full 20 ms
    /// windows become available. Linear interpolation is plenty for voice.
    fn process(&mut self) {
        let step = self.in_rate / SAMPLE_RATE as f64;
        while (self.pos as usize) + 1 < self.inbuf.len() {
            let i = self.pos as usize;
            let frac = (self.pos - i as f64) as f32;
            let s = self.inbuf[i] * (1.0 - frac) + self.inbuf[i + 1] * frac;
            self.frame.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
            self.pos += step;
            if self.frame.len() >= FRAME_SAMPLES {
                self.emit_frame();
            }
        }
        // Drop the input we've consumed, keeping the fractional remainder.
        let consumed = self.pos as usize;
        if consumed > 0 {
            self.inbuf.drain(0..consumed);
            self.pos -= consumed as f64;
        }
    }

    /// Encode one 20 ms frame and send it, but only while transmitting.
    fn emit_frame(&mut self) {
        let transmit = self.ptt.load(Ordering::Relaxed) && self.enabled.load(Ordering::Relaxed);
        if transmit {
            match self
                .encoder
                .encode(&self.frame[..FRAME_SAMPLES], &mut self.out)
            {
                Ok(n) => {
                    let _ = self.tx.send(Bytes::copy_from_slice(&self.out[..n]));
                }
                Err(e) => log::warn!("opus encode failed: {e}"),
            }
        }
        self.frame.drain(0..FRAME_SAMPLES);
    }
}
