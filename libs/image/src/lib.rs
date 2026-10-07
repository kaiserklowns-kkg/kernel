//! Image decoding for Oceans (ADR-0090), host-tested: PNG, with its own
//! DEFLATE inflater, and BMP; and what showing them needs (fitting them in
//! a window, scaling, transparency over a checkerboard).
//!
//! Image files are untrusted input. Every length and index is checked,
//! sizes are bounded ([`MAX_SIDE`], [`MAX_PIXELS`], [`MAX_FILE`]) before
//! memory is allocated, and decoding returns an [`Error`], never panics.

#![no_std]

extern crate alloc;

mod bmp;
pub mod inflate;
mod png;

use alloc::vec::Vec;

/// The largest width or height read.
pub const MAX_SIDE: u32 = 16_384;
/// The most pixels read (16 MiB of decoded pixels).
pub const MAX_PIXELS: u64 = 4 * 1024 * 1024;
/// The largest file (or compressed image data) read.
pub const MAX_FILE: usize = 32 * 1024 * 1024;

/// A decoded image: `width × height` pixels, rows top to bottom, each
/// `0xAARRGGBB` (alpha not premultiplied).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Bmp,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Bmp => "BMP",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Neither PNG nor BMP.
    UnknownFormat,
    /// The file ends too soon.
    Truncated,
    /// Something in the file is wrong.
    Corrupt(&'static str),
    /// Valid, but not something Oceans reads.
    Unsupported(&'static str),
    /// Larger than the limits.
    TooLarge,
}

impl Error {
    /// In words, for the person looking at it.
    pub fn message(self) -> &'static str {
        match self {
            Self::UnknownFormat => "not a PNG or BMP image",
            Self::Truncated => "the image file is cut short",
            Self::Corrupt(what) | Self::Unsupported(what) => what,
            Self::TooLarge => "the image is too large",
        }
    }
}

pub(crate) fn check_size(width: u32, height: u32) -> Result<(), Error> {
    if width == 0 || height == 0 {
        return Err(Error::Corrupt("an empty image"));
    }
    if width > MAX_SIDE || height > MAX_SIDE || u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Error::TooLarge);
    }
    Ok(())
}

/// Which format `bytes` are, by their first bytes.
pub fn format(bytes: &[u8]) -> Option<Format> {
    if png::is_png(bytes) {
        Some(Format::Png)
    } else if bmp::is_bmp(bytes) {
        Some(Format::Bmp)
    } else {
        None
    }
}

/// Decodes a PNG or BMP file.
pub fn decode(bytes: &[u8]) -> Result<(Format, Image), Error> {
    if bytes.len() > MAX_FILE {
        return Err(Error::TooLarge);
    }
    match format(bytes) {
        Some(Format::Png) => png::decode(bytes).map(|image| (Format::Png, image)),
        Some(Format::Bmp) => bmp::decode(bytes).map(|image| (Format::Bmp, image)),
        None => Err(Error::UnknownFormat),
    }
}

/// Whether a file name looks like an image Oceans reads.
pub fn is_image_name(name: &str) -> bool {
    let lower = |ext: &str| {
        name.len() > ext.len()
            && name.as_bytes()[name.len() - ext.len()..].eq_ignore_ascii_case(ext.as_bytes())
    };
    lower(".png") || lower(".bmp")
}

/// The size to show a `width × height` image at in a `box_w × box_h` box:
/// as large as fits, never larger than the image itself.
pub fn fit(width: u32, height: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    if width <= box_w && height <= box_h {
        return (width, height);
    }
    let (w, h) = (u64::from(width), u64::from(height));
    let (bw, bh) = (u64::from(box_w), u64::from(box_h));
    // The tighter of the two ratios.
    if w * bh > h * bw {
        (box_w, ((h * bw) / w).max(1) as u32)
    } else {
        (((w * bh) / h).max(1) as u32, box_h)
    }
}

/// `image` resampled to `width × height`: each output pixel the average of
/// the source pixels it covers (shrinking), or the nearest one (growing).
/// Alpha is averaged with the colours weighted by it.
pub fn scale(image: &Image, width: u32, height: u32) -> Vec<u32> {
    let (sw, sh) = (u64::from(image.width), u64::from(image.height));
    let mut out = Vec::with_capacity(width as usize * height as usize);
    for y in 0..u64::from(height) {
        let (y0, y1) = span(y, u64::from(height), sh);
        for x in 0..u64::from(width) {
            let (x0, x1) = span(x, u64::from(width), sw);
            let mut sum = [0u64; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = image.pixels[(sy * sw + sx) as usize];
                    let alpha = u64::from(p >> 24);
                    sum[0] += alpha;
                    sum[1] += alpha * u64::from((p >> 16) & 0xff);
                    sum[2] += alpha * u64::from((p >> 8) & 0xff);
                    sum[3] += alpha * u64::from(p & 0xff);
                }
            }
            let count = (y1 - y0) * (x1 - x0);
            out.push(if sum[0] == 0 {
                0
            } else {
                let a = sum[0] / count;
                let channel = |s: u64| (s / sum[0]) as u32;
                (a as u32) << 24 | channel(sum[1]) << 16 | channel(sum[2]) << 8 | channel(sum[3])
            });
        }
    }
    out
}

/// The source pixels output pixel `i` (of `out`) covers (of `size`): at
/// least one.
fn span(i: u64, out: u64, size: u64) -> (u64, u64) {
    let start = i * size / out;
    let end = ((i + 1) * size / out).max(start + 1).min(size);
    (start.min(size - 1), end)
}

/// `pixel` over the checkerboard that shows transparency, at screen
/// position (`x`, `y`): opaque `0x00RRGGBB`.
pub fn over_checker(pixel: u32, x: i32, y: i32) -> u32 {
    let alpha = pixel >> 24;
    if alpha == 0xff {
        return pixel & 0x00ff_ffff;
    }
    let square = ((x >> 3) + (y >> 3)) & 1 == 0;
    let under: u32 = if square { 0xff } else { 0xcc };
    let mix = |shift: u32| {
        let top = (pixel >> shift) & 0xff;
        (top * alpha + under * (255 - alpha)) / 255
    };
    mix(16) << 16 | mix(8) << 8 | mix(0)
}

#[cfg(test)]
mod tests;
