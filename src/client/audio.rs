//! Background music playback.
//!
//! Each dimension has its own pool of looping tracks under
//! `assets/music/<dimension>/<n>.ogg`, embedded into the binary via
//! [`crate::assets`]. When the player enters a dimension we pick one of its
//! tracks at random and loop it, played quieter than the source file so it sits
//! *under* the game rather than over it.
//!
//! The randomness is intentionally trivial today (every dimension ships a single
//! track), but the selection already fans out over however many tracks a
//! dimension lists, so dropping more `.ogg` files in beside the first is all it
//! takes to get variety.

use std::io::Cursor;

use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};

use crate::world::Dimension;

/// Music plays at this fraction of the source track's volume, so it stays in the
/// background rather than drowning out the rest of the game.
const MUSIC_VOLUME: f32 = 0.3;

/// Owns the audio output and the currently looping music track.
pub struct Music {
    // The output stream must stay alive for sound to keep playing; dropping it
    // silences everything. We never touch it directly after construction.
    _stream: OutputStream,
    handle: OutputStreamHandle,
    /// The sink driving the current loop, if any. Dropping it stops the music.
    sink: Option<Sink>,
    /// Which dimension's music is playing, so a redundant dimension update
    /// doesn't restart (and re-randomise) the track.
    current: Option<Dimension>,
}

impl Music {
    /// Open the default audio device. Returns `None` (with a warning) if no
    /// device is available, so the game runs fine on headless/muted machines.
    pub fn new() -> Option<Self> {
        match OutputStream::try_default() {
            Ok((stream, handle)) => Some(Music {
                _stream: stream,
                handle,
                sink: None,
                current: None,
            }),
            Err(e) => {
                log::warn!("audio unavailable, music disabled: {e:#}");
                None
            }
        }
    }

    /// Start (or switch to) the music for `dim`, looping a randomly chosen track
    /// for that dimension. A no-op if that dimension's music is already playing.
    pub fn play_for(&mut self, dim: Dimension) {
        if self.current == Some(dim) && self.sink.as_ref().is_some_and(|s| !s.empty()) {
            return;
        }

        let name = music_dir(dim);
        let count = crate::assets::music_track_count(name);
        if count == 0 {
            self.stop();
            return;
        }
        let track = random_below(count);

        let Some(bytes) = crate::assets::music_ogg(name, track) else {
            return;
        };
        let decoder = match Decoder::new(Cursor::new(bytes)) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("could not decode music {name}/{track}: {e:#}");
                return;
            }
        };
        let sink = match Sink::try_new(&self.handle) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("could not start music: {e:#}");
                return;
            }
        };
        sink.set_volume(MUSIC_VOLUME);
        sink.append(decoder.repeat_infinite());

        self.sink = Some(sink);
        self.current = Some(dim);
    }

    /// Stop any playing music (e.g. on leaving a world).
    pub fn stop(&mut self) {
        self.sink = None;
        self.current = None;
    }
}

/// The asset subdirectory holding a dimension's music tracks.
fn music_dir(dim: Dimension) -> &'static str {
    match dim {
        Dimension::Overworld => "overworld",
        Dimension::Underworld => "underworld",
        // The arena ships with no music of its own yet; `music_track_count` returns
        // 0 for an unknown dir, so this simply plays silence there.
        Dimension::Arena => "arena",
    }
}

/// A random integer in `[0, n)`. Falls back to `0` if the OS RNG is unavailable,
/// which is harmless — it just means the first track is always chosen.
fn random_below(n: u32) -> u32 {
    if n <= 1 {
        return 0;
    }
    let mut buf = [0u8; 4];
    if getrandom::fill(&mut buf).is_err() {
        return 0;
    }
    u32::from_le_bytes(buf) % n
}
