//! Voice-chat constants and the wire type shared between client and server.
//!
//! Voice runs over a separate [MOQ](https://moq.dev) (Media over QUIC) endpoint,
//! distinct from the game's own QUIC connection. It is entirely optional and only
//! ever active when the server owner enables it (see [`crate::server`]'s
//! `enable_voice` and the dedicated-server `voice` flag). When enabled, the game
//! server runs an in-process MOQ relay; each client publishes its microphone as a
//! single Opus track (framed by [`hang`]) and subscribes to every other player's
//! track, so audio is global — everyone hears everyone.
//!
//! The actual capture/encode/playback lives in [`crate::client::voice`]; the relay
//! lives in [`crate::server`]. This module only holds the values both sides must
//! agree on plus [`VoiceInfo`], which the server hands a joining client so it can
//! find and trust the relay.

use serde::{Deserialize, Serialize};

use crate::entity::EntityId;

/// Opus is defined at 48 kHz; we capture and play back mono to halve the bitrate
/// (voice doesn't need stereo).
pub const SAMPLE_RATE: u32 = 48_000;

/// Mono — one channel everywhere (capture, Opus, playback).
pub const CHANNELS: u16 = 1;

/// Samples per Opus frame: a 20 ms frame at 48 kHz, the standard VoIP
/// packetisation. The capture side buffers exactly this many mono samples before
/// encoding one packet.
pub const FRAME_SAMPLES: usize = 960;

/// Microseconds of audio carried by one [`FRAME_SAMPLES`] frame, used as the
/// increment for the monotonic [`hang`] frame timestamps.
pub const FRAME_MICROS: u64 = 20_000;

/// Name of the single Opus track inside every player's voice broadcast. Both the
/// publisher and the subscriber hard-code this, so no catalog lookup is needed.
pub const AUDIO_TRACK: &str = "audio";

/// Path prefix under which the relay announces each player's voice broadcast. The
/// full path is [`broadcast_path`]; subscribers match on this prefix.
pub const BROADCAST_PREFIX: &str = "voice/";

/// The MOQ broadcast path for the player with `entity_id`: `voice/<id>`. Each
/// client publishes under its own id and skips this path when subscribing so it
/// never plays back its own voice.
pub fn broadcast_path(entity_id: EntityId) -> String {
    format!("{BROADCAST_PREFIX}{entity_id}")
}

/// Parse the [`EntityId`] back out of a [`broadcast_path`], or `None` if the path
/// isn't one of ours. Subscribers use it to label who is speaking.
pub fn entity_from_path(path: &str) -> Option<EntityId> {
    path.strip_prefix(BROADCAST_PREFIX)?.parse().ok()
}

/// What the server tells a joining client about its optional voice relay, carried
/// in [`crate::protocol::ServerMessage::Welcome`]. `None` there means the owner
/// left voice disabled, so the client shows no voice UI and never opens a relay
/// connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceInfo {
    /// UDP port of the MOQ relay. The relay shares the game server's host/IP, so
    /// the client combines this with the address it already connected to.
    pub port: u16,
    /// Hex-encoded SHA-256 of the relay's self-signed certificate. The relay mints
    /// its own certificate (independent of the game identity), so the client pins
    /// exactly this fingerprint for the voice connection rather than re-running a
    /// trust-on-first-use prompt.
    pub cert_hash: String,
}
