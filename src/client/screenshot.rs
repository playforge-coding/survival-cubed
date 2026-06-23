//! Saving in-game screenshots: a full-resolution PNG of the world (the HUD is
//! excluded by the renderer, see [`super::render::Gfx::render`]) plus a
//! compressed, web-friendly JPEG re-encode. Both land under the platform data
//! dir so they're easy to find and share.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use image::{ImageEncoder, RgbaImage};

/// JPEG quality (0-100) for the web-friendly copy. 80 keeps the file small for
/// sharing while staying visually close to the PNG.
const JPEG_QUALITY: u8 = 80;

/// Directory screenshots are written to:
/// `~/.local/share/survival-cubed/screenshots`, falling back to `./screenshots`
/// if no data dir is known.
pub fn screenshots_dir() -> PathBuf {
    let mut p = dirs::data_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("survival-cubed");
    p.push("screenshots");
    p
}

/// Write `rgba` (row-major, `width`x`height`, 4 bytes/pixel) to disk as a
/// lossless PNG and a smaller JPEG re-encode. Returns the PNG path on success.
///
/// This does CPU encoding and disk I/O, so callers should run it off the render
/// thread (the pixel buffer is owned, so it moves cheaply to a worker thread).
pub fn save(rgba: Vec<u8>, width: u32, height: u32) -> io::Result<PathBuf> {
    let dir = screenshots_dir();
    std::fs::create_dir_all(&dir)?;

    // Millisecond timestamp keeps filenames sortable and collision-free across
    // rapid F2 presses.
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let png_path = dir.join(format!("screenshot-{stamp}.png"));
    let jpg_path = dir.join(format!("screenshot-{stamp}.jpg"));

    let img = RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "screenshot buffer size mismatch")
    })?;

    // Full-quality PNG (format inferred from the .png extension).
    img.save(&png_path).map_err(to_io)?;

    // Web-friendly JPEG: JPEG has no alpha, so flatten to RGB, then compress.
    write_jpeg(&jpg_path, &img, width, height)?;

    Ok(png_path)
}

/// Re-encode `img` as a compressed JPEG at [`JPEG_QUALITY`].
fn write_jpeg(path: &Path, img: &RgbaImage, width: u32, height: u32) -> io::Result<()> {
    let rgb = image::DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    let file = io::BufWriter::new(std::fs::File::create(path)?);
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(file, JPEG_QUALITY);
    encoder
        .write_image(rgb.as_raw(), width, height, image::ExtendedColorType::Rgb8)
        .map_err(to_io)
}

fn to_io(e: image::ImageError) -> io::Error {
    io::Error::other(e)
}
