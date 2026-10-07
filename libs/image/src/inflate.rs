//! DEFLATE (RFC 1951) and the zlib wrapper around it (RFC 1950), as PNG
//! uses them. Decoding follows the reference decoder `puff` (Mark Adler):
//! canonical Huffman codes decoded a bit at a time, every length and
//! distance checked, output bounded.

use alloc::vec::Vec;

use crate::Error;

const MAX_BITS: usize = 15;
const MAX_LITERAL_CODES: usize = 286;
const MAX_DISTANCE_CODES: usize = 30;
const FIXED_LITERAL_CODES: usize = 288;

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// The order code length codes are sent in.
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Bits, least significant first.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
    buffer: u32,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            at: 0,
            buffer: 0,
            count: 0,
        }
    }

    fn bits(&mut self, need: u32) -> Result<u32, Error> {
        while self.count < need {
            let byte = *self.data.get(self.at).ok_or(Error::Truncated)?;
            self.at += 1;
            self.buffer |= u32::from(byte) << self.count;
            self.count += 8;
        }
        let value = self.buffer & ((1u32 << need) - 1);
        self.buffer >>= need;
        self.count -= need;
        Ok(value)
    }

    /// Drops the rest of the current byte.
    fn align(&mut self) {
        self.buffer = 0;
        self.count = 0;
    }

    /// The bytes after the last one used.
    fn rest(&self) -> &'a [u8] {
        &self.data[self.at..]
    }
}

/// A canonical Huffman code: how many codes of each length, and the
/// symbols in code order.
struct Huffman {
    count: [u16; MAX_BITS + 1],
    symbol: [u16; FIXED_LITERAL_CODES],
}

impl Huffman {
    /// Builds the code from each symbol's code length (0: unused).
    /// Over-subscribed sets are refused; incomplete ones are allowed (as
    /// zlib does), and fail only if an unused code is met.
    fn new(lengths: &[u8]) -> Result<Self, Error> {
        let mut huffman = Self {
            count: [0; MAX_BITS + 1],
            symbol: [0; FIXED_LITERAL_CODES],
        };
        for &length in lengths {
            huffman.count[usize::from(length)] += 1;
        }
        if usize::from(huffman.count[0]) == lengths.len() {
            // No codes at all: fine until one is needed.
            return Ok(huffman);
        }
        let mut left: i32 = 1;
        for length in 1..=MAX_BITS {
            left <<= 1;
            left -= i32::from(huffman.count[length]);
            if left < 0 {
                return Err(Error::Corrupt("an over-subscribed Huffman code"));
            }
        }
        let mut offsets = [0u16; MAX_BITS + 1];
        for length in 1..MAX_BITS {
            offsets[length + 1] = offsets[length] + huffman.count[length];
        }
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let at = &mut offsets[usize::from(length)];
                huffman.symbol[usize::from(*at)] = symbol as u16;
                *at += 1;
            }
        }
        Ok(huffman)
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<u16, Error> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for length in 1..=MAX_BITS {
            code |= bits.bits(1)? as i32;
            let count = i32::from(self.count[length]);
            if code - count < first {
                return Ok(self.symbol[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(Error::Corrupt("an unused Huffman code"))
    }
}

/// Inflates raw DEFLATE `data`, producing at most `limit` bytes. Returns
/// the output and the bytes after the last block.
pub fn inflate(data: &[u8], limit: usize) -> Result<(Vec<u8>, &[u8]), Error> {
    let mut bits = Bits::new(data);
    let mut out = Vec::new();
    loop {
        let last = bits.bits(1)? == 1;
        match bits.bits(2)? {
            0 => stored(&mut bits, &mut out, limit)?,
            1 => {
                let (literals, distances) = fixed_codes()?;
                codes(&mut bits, &mut out, limit, &literals, &distances)?;
            }
            2 => {
                let (literals, distances) = dynamic_codes(&mut bits)?;
                codes(&mut bits, &mut out, limit, &literals, &distances)?;
            }
            _ => return Err(Error::Corrupt("a reserved DEFLATE block type")),
        }
        if last {
            bits.align();
            return Ok((out, bits.rest()));
        }
    }
}

fn stored(bits: &mut Bits<'_>, out: &mut Vec<u8>, limit: usize) -> Result<(), Error> {
    bits.align();
    let header = bits
        .data
        .get(bits.at..bits.at + 4)
        .ok_or(Error::Truncated)?;
    let len = usize::from(u16::from_le_bytes([header[0], header[1]]));
    let check = u16::from_le_bytes([header[2], header[3]]);
    if check != !(len as u16) {
        return Err(Error::Corrupt("a stored block's length check"));
    }
    bits.at += 4;
    let bytes = bits
        .data
        .get(bits.at..bits.at + len)
        .ok_or(Error::Truncated)?;
    if out.len() + len > limit {
        return Err(Error::Corrupt("more image data than its size"));
    }
    out.extend_from_slice(bytes);
    bits.at += len;
    Ok(())
}

fn fixed_codes() -> Result<(Huffman, Huffman), Error> {
    let mut lengths = [0u8; FIXED_LITERAL_CODES];
    for (symbol, length) in lengths.iter_mut().enumerate() {
        *length = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Ok((
        Huffman::new(&lengths)?,
        Huffman::new(&[5; MAX_DISTANCE_CODES])?,
    ))
}

fn dynamic_codes(bits: &mut Bits<'_>) -> Result<(Huffman, Huffman), Error> {
    let literal_count = bits.bits(5)? as usize + 257;
    let distance_count = bits.bits(5)? as usize + 1;
    let length_count = bits.bits(4)? as usize + 4;
    if literal_count > MAX_LITERAL_CODES || distance_count > MAX_DISTANCE_CODES {
        return Err(Error::Corrupt("too many Huffman codes"));
    }
    let mut code_lengths = [0u8; 19];
    for &at in CODE_LENGTH_ORDER.iter().take(length_count) {
        code_lengths[at] = bits.bits(3)? as u8;
    }
    let lengths_code = Huffman::new(&code_lengths)?;
    let mut lengths = [0u8; MAX_LITERAL_CODES + MAX_DISTANCE_CODES];
    let total = literal_count + distance_count;
    let mut at = 0;
    while at < total {
        let symbol = lengths_code.decode(bits)?;
        if symbol < 16 {
            lengths[at] = symbol as u8;
            at += 1;
            continue;
        }
        let (value, repeat) = match symbol {
            16 => {
                let previous = *lengths
                    .get(at.wrapping_sub(1))
                    .filter(|_| at > 0)
                    .ok_or(Error::Corrupt("a repeat with nothing before it"))?;
                (previous, 3 + bits.bits(2)? as usize)
            }
            17 => (0, 3 + bits.bits(3)? as usize),
            _ => (0, 11 + bits.bits(7)? as usize),
        };
        if at + repeat > total {
            return Err(Error::Corrupt("too many code lengths"));
        }
        lengths[at..at + repeat].fill(value);
        at += repeat;
    }
    if lengths[256] == 0 {
        return Err(Error::Corrupt("no end-of-block code"));
    }
    Ok((
        Huffman::new(&lengths[..literal_count])?,
        Huffman::new(&lengths[literal_count..total])?,
    ))
}

fn codes(
    bits: &mut Bits<'_>,
    out: &mut Vec<u8>,
    limit: usize,
    literals: &Huffman,
    distances: &Huffman,
) -> Result<(), Error> {
    loop {
        let symbol = literals.decode(bits)?;
        match symbol {
            0..=255 => {
                if out.len() == limit {
                    return Err(Error::Corrupt("more image data than its size"));
                }
                out.push(symbol as u8);
            }
            256 => return Ok(()),
            _ => {
                let index = usize::from(symbol - 257);
                if index >= LENGTH_BASE.len() {
                    return Err(Error::Corrupt("a bad length code"));
                }
                let length = usize::from(LENGTH_BASE[index])
                    + bits.bits(u32::from(LENGTH_EXTRA[index]))? as usize;
                let index = usize::from(distances.decode(bits)?);
                if index >= DISTANCE_BASE.len() {
                    return Err(Error::Corrupt("a bad distance code"));
                }
                let distance = usize::from(DISTANCE_BASE[index])
                    + bits.bits(u32::from(DISTANCE_EXTRA[index]))? as usize;
                if distance > out.len() {
                    return Err(Error::Corrupt("a distance before the start"));
                }
                if out.len() + length > limit {
                    return Err(Error::Corrupt("more image data than its size"));
                }
                let from = out.len() - distance;
                // Byte by byte: a copy may overlap what it produces.
                for i in 0..length {
                    let byte = out[from + i];
                    out.push(byte);
                }
            }
        }
    }
}

/// Inflates a zlib stream, checking its header and Adler-32.
pub fn zlib(data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let header = data.get(..2).ok_or(Error::Truncated)?;
    let (cmf, flg) = (header[0], header[1]);
    if cmf & 0x0f != 8 || cmf >> 4 > 7 || (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0 {
        return Err(Error::Corrupt("not a zlib stream"));
    }
    if flg & 0x20 != 0 {
        return Err(Error::Unsupported("a preset zlib dictionary"));
    }
    let (out, rest) = inflate(&data[2..], limit)?;
    let check = rest.get(..4).ok_or(Error::Truncated)?;
    if u32::from_be_bytes([check[0], check[1], check[2], check[3]]) != adler32(&out) {
        return Err(Error::Corrupt("the image data's checksum"));
    }
    Ok(out)
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}
