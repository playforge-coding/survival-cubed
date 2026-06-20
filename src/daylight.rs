//! Shared day/night clock.
//!
//! The authoritative server owns the wall clock (so every client agrees on the
//! time and on when hostile creatures wake up); clients are told the current
//! [normalized time of day](TimeOfDay) and advance it locally between updates
//! to keep the sky tint smooth. Both sides use the helpers here so the world
//! darkens and the slimes turn hostile in lockstep.

/// Length of one full day/night cycle, in seconds (20 minutes).
pub const DAY_LENGTH_SECS: f32 = 20.0 * 60.0;

/// How dark midnight gets (0 = black, 1 = full daylight). The world never goes
/// fully black so the player can still see what they are doing.
const NIGHT_BRIGHTNESS: f32 = 0.22;

/// Below this daylight level the world counts as night and slimes hunt.
const NIGHT_THRESHOLD: f32 = 0.45;

/// Daylight level in `[NIGHT_BRIGHTNESS, 1.0]` for a normalized time of day `t`
/// in `[0, 1)`, where `0.0` is sunrise, `0.25` noon, `0.5` sunset and `0.75`
/// midnight. Smoothly bright at noon and dark at midnight.
pub fn brightness(t: f32) -> f32 {
    let day = 0.5 + 0.5 * (std::f32::consts::TAU * (t - 0.25)).cos();
    NIGHT_BRIGHTNESS + (1.0 - NIGHT_BRIGHTNESS) * day
}

/// Whether it is night — when slimes turn hostile and hunt players.
pub fn is_night(t: f32) -> bool {
    brightness(t) < NIGHT_THRESHOLD
}

/// Background sky color (RGBA) for a normalized time of day, fading the daytime
/// blue toward a near-black night.
pub fn sky_color(t: f32) -> [f32; 4] {
    const DAY: [f32; 3] = [0.45, 0.62, 0.86];
    const NIGHT: [f32; 3] = [0.02, 0.03, 0.09];
    // Remap brightness onto 0 (midnight) .. 1 (noon) for the lerp.
    let f = ((brightness(t) - NIGHT_BRIGHTNESS) / (1.0 - NIGHT_BRIGHTNESS)).clamp(0.0, 1.0);
    [
        NIGHT[0] + (DAY[0] - NIGHT[0]) * f,
        NIGHT[1] + (DAY[1] - NIGHT[1]) * f,
        NIGHT[2] + (DAY[2] - NIGHT[2]) * f,
        1.0,
    ]
}

/// Wrap an arbitrary elapsed-seconds count into a normalized time of day.
pub fn time_of_day(elapsed_secs: f32) -> f32 {
    (elapsed_secs / DAY_LENGTH_SECS).rem_euclid(1.0)
}
