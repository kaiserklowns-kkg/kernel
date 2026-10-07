//! PNG (ISO/IEC 15948): every colour type and bit depth, Adam7
//! interlacing, transparency (`tRNS`). Chunk CRCs and the zlib checksum are
//! checked; ancillary chunks Oceans does not use are skipped.

use alloc::vec::Vec;

use crate::{Error, Image, inflate};

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
/// Adam7: first column, first row, column step, row step of each pass.
const ADAM7: [(u32, u32, u32, u32); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&SIGNATURE)
}

#[derive(Clone, Copy)]
struct Header {
    width: u32,
    height: u32,
    depth: u8,
    colour: u8,
    interlaced: bool,
}

impl Header {
    fn channels(&self) -> usize {
        match self.colour {
            0 | 3 => 1,
            4 => 2,
            2 => 3,
            _ => 4,
        }
    }

    /// Bytes of one row of `width` pixels, without the filter byte.
    fn stride(&self, width: u32) -> usize {
        (width as usize * self.channels() * usize::from(self.depth)).div_ceil(8)
    }

    /// Bytes per complete pixel for filtering (at least 1).
    fn filter_step(&self) -> usize {
        (self.channels() * usize::from(self.depth))
            .div_ceil(8)
            .max(1)
    }
}

/// Transparency, from `tRNS`.
enum Transparency {
    None,
    /// One gray value (16-bit range) is transparent.
    Gray(u16),
    Rgb(u16, u16, u16),
    /// The palette's alphas (missing entries opaque).
    Palette(Vec<u8>),
}

pub fn decode(bytes: &[u8]) -> Result<Image, Error> {
    let mut rest = bytes.get(SIGNATURE.len()..).ok_or(Error::Truncated)?;
    let mut header: Option<Header> = None;
    let mut palette: Vec<u32> = Vec::new();
    let mut transparency = Transparency::None;
    let mut data = Vec::new();
    let mut ended = false;
    while !ended {
        let length_bytes = rest.get(..4).ok_or(Error::Truncated)?;
        let length = u32::from_be_bytes(length_bytes.try_into().unwrap()) as usize;
        let chunk = rest.get(4..8 + length).ok_or(Error::Truncated)?;
        let crc = rest.get(8 + length..12 + length).ok_or(Error::Truncated)?;
        if u32::from_be_bytes(crc.try_into().unwrap()) != crc32(chunk) {
            return Err(Error::Corrupt("a chunk's checksum"));
        }
        let (kind, body) = chunk.split_at(4);
        rest = &rest[12 + length..];
        match kind {
            b"IHDR" => header = Some(parse_header(body)?),
            _ if header.is_none() => return Err(Error::Corrupt("no header first")),
            b"PLTE" => {
                if body.len() % 3 != 0 || body.is_empty() || body.len() > 3 * 256 {
                    return Err(Error::Corrupt("the palette's size"));
                }
                palette = body
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|&[r, g, b]| {
                        0xff00_0000 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
                    })
                    .collect();
            }
            b"tRNS" => {
                let word = |at: usize| -> Result<u16, Error> {
                    let b = body.get(at..at + 2).ok_or(Error::Truncated)?;
                    Ok(u16::from_be_bytes([b[0], b[1]]))
                };
                transparency = match header.map(|h| h.colour) {
                    Some(0) => Transparency::Gray(word(0)?),
                    Some(2) => Transparency::Rgb(word(0)?, word(2)?, word(4)?),
                    Some(3) => Transparency::Palette(body.to_vec()),
                    _ => return Err(Error::Corrupt("transparency for a colour type with alpha")),
                };
            }
            b"IDAT" => {
                if data.len() + body.len() > crate::MAX_FILE {
                    return Err(Error::TooLarge);
                }
                data.extend_from_slice(body);
            }
            b"IEND" => ended = true,
            // Critical chunks Oceans does not know cannot be skipped.
            _ if kind[0] & 0x20 == 0 => {
                return Err(Error::Unsupported("an unknown critical chunk"));
            }
            _ => {}
        }
    }
    let header = header.ok_or(Error::Corrupt("no header"))?;
    if header.colour == 3 && palette.is_empty() {
        return Err(Error::Corrupt("a palette image without a palette"));
    }
    let raw_size = passes(&header)
        .map(|(w, h)| {
            if w == 0 || h == 0 {
                0
            } else {
                h as usize * (1 + header.stride(w))
            }
        })
        .sum();
    let raw = inflate::zlib(&data, raw_size)?;
    if raw.len() != raw_size {
        return Err(Error::Corrupt("less image data than its size"));
    }
    if let Transparency::Palette(alphas) = &transparency {
        for (entry, &alpha) in palette.iter_mut().zip(alphas) {
            *entry = (*entry & 0x00ff_ffff) | u32::from(alpha) << 24;
        }
    }

    let mut pixels = alloc::vec![0u32; header.width as usize * header.height as usize];
    let mut at = 0;
    for (pass, (w, h)) in passes(&header).enumerate() {
        if w == 0 || h == 0 {
            continue;
        }
        let stride = header.stride(w);
        let size = h as usize * (1 + stride);
        let mut rows = raw[at..at + size].to_vec();
        at += size;
        unfilter(&mut rows, stride, header.filter_step())?;
        let (x0, y0, dx, dy) = if header.interlaced {
            ADAM7[pass]
        } else {
            (0, 0, 1, 1)
        };
        for row in 0..h {
            let line = &rows[row as usize * (1 + stride) + 1..(row as usize + 1) * (1 + stride)];
            let y = y0 + row * dy;
            for column in 0..w {
                let x = x0 + column * dx;
                pixels[(y * header.width + x) as usize] =
                    pixel(&header, line, column as usize, &palette, &transparency)?;
            }
        }
    }
    Ok(Image {
        width: header.width,
        height: header.height,
        pixels,
    })
}

fn parse_header(body: &[u8]) -> Result<Header, Error> {
    if body.len() != 13 {
        return Err(Error::Corrupt("the header's size"));
    }
    let width = u32::from_be_bytes(body[0..4].try_into().unwrap());
    let height = u32::from_be_bytes(body[4..8].try_into().unwrap());
    let (depth, colour) = (body[8], body[9]);
    let valid = match colour {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        _ => false,
    };
    if !valid {
        return Err(Error::Corrupt("the colour type and depth"));
    }
    if body[10] != 0 || body[11] != 0 || body[12] > 1 {
        return Err(Error::Unsupported(
            "a compression, filter or interlace method",
        ));
    }
    crate::check_size(width, height)?;
    Ok(Header {
        width,
        height,
        depth,
        colour,
        interlaced: body[12] == 1,
    })
}

/// Each pass's size in pixels (one pass if not interlaced).
fn passes(header: &Header) -> impl Iterator<Item = (u32, u32)> + '_ {
    let all = if header.interlaced {
        &ADAM7[..]
    } else {
        &ADAM7[..1]
    };
    all.iter().map(move |&(x0, y0, dx, dy)| {
        if !header.interlaced {
            return (header.width, header.height);
        }
        let span = |size: u32, start: u32, step: u32| {
            if size > start {
                (size - start).div_ceil(step)
            } else {
                0
            }
        };
        (span(header.width, x0, dx), span(header.height, y0, dy))
    })
}

/// Undoes the row filters in place (`rows`: filter byte, then `stride`
/// bytes, per row).
fn unfilter(rows: &mut [u8], stride: usize, step: usize) -> Result<(), Error> {
    let line = 1 + stride;
    for row in 0..rows.len() / line {
        let start = row * line;
        let kind = rows[start];
        for i in 0..stride {
            let at = start + 1 + i;
            let left = if i >= step { rows[at - step] } else { 0 };
            let up = if row > 0 { rows[at - line] } else { 0 };
            let corner = if row > 0 && i >= step {
                rows[at - line - step]
            } else {
                0
            };
            let predicted = match kind {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => paeth(left, up, corner),
                _ => return Err(Error::Corrupt("an unknown row filter")),
            };
            rows[at] = rows[at].wrapping_add(predicted);
        }
    }
    Ok(())
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Pixel `x` of an unfiltered row, as 0xAARRGGBB.
fn pixel(
    header: &Header,
    line: &[u8],
    x: usize,
    palette: &[u32],
    transparency: &Transparency,
) -> Result<u32, Error> {
    let depth = usize::from(header.depth);
    // Sample `channel` of this pixel, full range (up to 16 bits).
    let sample = |channel: usize| -> u16 {
        let index = x * header.channels() + channel;
        match depth {
            16 => u16::from_be_bytes([line[2 * index], line[2 * index + 1]]),
            8 => u16::from(line[index]),
            _ => {
                let bit = index * depth;
                let byte = line[bit / 8];
                let shift = 8 - depth - bit % 8;
                u16::from(byte >> shift) & ((1 << depth) - 1)
            }
        }
    };
    // To 8 bits.
    let eight = |value: u16| -> u32 {
        match depth {
            16 => u32::from(value >> 8),
            8 => u32::from(value),
            _ => u32::from(value) * 255 / ((1 << depth) - 1),
        }
    };
    let rgb = |r: u32, g: u32, b: u32| r << 16 | g << 8 | b;
    Ok(match header.colour {
        0 => {
            let gray = sample(0);
            let alpha = match transparency {
                Transparency::Gray(key) if *key == gray => 0,
                _ => 0xff,
            };
            let g = eight(gray);
            alpha << 24 | rgb(g, g, g)
        }
        2 => {
            let (r, g, b) = (sample(0), sample(1), sample(2));
            let alpha = match transparency {
                Transparency::Rgb(kr, kg, kb) if (*kr, *kg, *kb) == (r, g, b) => 0,
                _ => 0xff,
            };
            alpha << 24 | rgb(eight(r), eight(g), eight(b))
        }
        3 => *palette
            .get(usize::from(sample(0)))
            .ok_or(Error::Corrupt("a colour beyond the palette"))?,
        4 => {
            let g = eight(sample(0));
            eight(sample(1)) << 24 | rgb(g, g, g)
        }
        _ => eight(sample(3)) << 24 | rgb(eight(sample(0)), eight(sample(1)), eight(sample(2))),
    })
}

/// CRC-32 (ISO 3309, as PNG and zlib's gzip use it).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
