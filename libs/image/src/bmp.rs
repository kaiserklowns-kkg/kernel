//! Windows bitmaps (BMP): 1, 4 and 8 bits with a palette, 16 bits (5-5-5,
//! or bit fields), 24 bits, 32 bits (with bit fields, alpha when a mask
//! names it). Bottom-up and top-down. Compressed (RLE) bitmaps are not
//! read.

use crate::{Error, Image};

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

pub fn is_bmp(bytes: &[u8]) -> bool {
    bytes.starts_with(b"BM")
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Error> {
    let b = bytes.get(at..at + 2).ok_or(Error::Truncated)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let b = bytes.get(at..at + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A channel given by a bit mask: where it is and how wide.
#[derive(Clone, Copy)]
struct Mask {
    mask: u32,
    shift: u32,
    bits: u32,
}

impl Mask {
    fn new(mask: u32) -> Self {
        if mask == 0 {
            return Self {
                mask,
                shift: 0,
                bits: 0,
            };
        }
        let shift = mask.trailing_zeros();
        Self {
            mask,
            shift,
            bits: (mask >> shift).trailing_ones(),
        }
    }

    /// The channel of `value`, scaled to 8 bits (`absent` without a mask).
    fn get(self, value: u32, absent: u32) -> u32 {
        if self.bits == 0 {
            return absent;
        }
        let raw = (value & self.mask) >> self.shift;
        let max = (1u64 << self.bits) - 1;
        (u64::from(raw) * 255 / max) as u32
    }
}

pub fn decode(bytes: &[u8]) -> Result<Image, Error> {
    let offset = u32_at(bytes, 10)? as usize;
    let header_size = u32_at(bytes, 14)? as usize;
    if header_size < 40 {
        return Err(Error::Unsupported("an OS/2 bitmap header"));
    }
    let width = u32_at(bytes, 18)? as i32;
    let height = u32_at(bytes, 22)? as i32;
    let bpp = u16_at(bytes, 28)?;
    let compression = u32_at(bytes, 30)?;
    let colours_used = u32_at(bytes, 46)? as usize;
    if width <= 0 || height == 0 || height == i32::MIN {
        return Err(Error::Corrupt("the bitmap's size"));
    }
    let (width, top_down, height) = (width as u32, height < 0, height.unsigned_abs());
    crate::check_size(width, height)?;

    let masks = match (compression, bpp) {
        (BI_RGB, 16) => Some([0x7c00, 0x03e0, 0x001f, 0]),
        (BI_RGB, 32) => Some([0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0]),
        (BI_RGB, 1 | 4 | 8 | 24) => None,
        (BI_BITFIELDS | BI_ALPHABITFIELDS, 16 | 32) => {
            // In the header (V2 and later), or right after a 40-byte one.
            let alpha = if header_size >= 56 || compression == BI_ALPHABITFIELDS {
                u32_at(bytes, 54 + 12)?
            } else {
                0
            };
            Some([
                u32_at(bytes, 54)?,
                u32_at(bytes, 58)?,
                u32_at(bytes, 62)?,
                alpha,
            ])
        }
        (BI_RGB, _) => return Err(Error::Unsupported("this bitmap depth")),
        _ => return Err(Error::Unsupported("a compressed bitmap")),
    };

    // The palette follows the header (and the masks, after a 40-byte header).
    let palette_at = 14
        + header_size
        + if compression == BI_BITFIELDS && header_size == 40 {
            12
        } else {
            0
        };
    let mut palette = [0xff00_0000u32; 256];
    if bpp <= 8 {
        // What the header says, but never past the pixel data: files often
        // say 0 (all of them) and carry fewer.
        let said = if colours_used == 0 {
            1 << bpp
        } else {
            colours_used.min(256)
        };
        let count = said.min(offset.saturating_sub(palette_at) / 4);
        for (i, entry) in palette.iter_mut().take(count).enumerate() {
            let at = palette_at + 4 * i;
            let b = bytes.get(at..at + 3).ok_or(Error::Truncated)?;
            *entry = 0xff00_0000 | u32::from(b[2]) << 16 | u32::from(b[1]) << 8 | u32::from(b[0]);
        }
    }

    let stride = (width as usize * usize::from(bpp)).div_ceil(32) * 4;
    let end = offset
        .checked_add(stride * height as usize)
        .ok_or(Error::TooLarge)?;
    let data = bytes.get(offset..end).ok_or(Error::Truncated)?;
    let [r, g, b, a] = masks.map_or([Mask::new(0); 4], |m| m.map(Mask::new));
    let mut pixels = alloc::vec![0u32; width as usize * height as usize];
    for row in 0..height as usize {
        let y = if top_down {
            row
        } else {
            height as usize - 1 - row
        };
        let line = &data[row * stride..(row + 1) * stride];
        for x in 0..width as usize {
            let colour = match bpp {
                1 | 4 | 8 => {
                    let bits = usize::from(bpp);
                    let bit = x * bits;
                    let index = (line[bit / 8] >> (8 - bits - bit % 8)) & ((1 << bits) - 1) as u8;
                    palette[usize::from(index)]
                }
                24 => {
                    let p = &line[3 * x..3 * x + 3];
                    0xff00_0000 | u32::from(p[2]) << 16 | u32::from(p[1]) << 8 | u32::from(p[0])
                }
                _ => {
                    let value = if bpp == 16 {
                        u32::from(u16::from_le_bytes([line[2 * x], line[2 * x + 1]]))
                    } else {
                        u32::from_le_bytes(line[4 * x..4 * x + 4].try_into().unwrap())
                    };
                    a.get(value, 0xff) << 24
                        | r.get(value, 0) << 16
                        | g.get(value, 0) << 8
                        | b.get(value, 0)
                }
            };
            pixels[y * width as usize + x] = colour;
        }
    }
    Ok(Image {
        width,
        height,
        pixels,
    })
}
