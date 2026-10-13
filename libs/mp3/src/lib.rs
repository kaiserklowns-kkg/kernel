//! MP3 for Oceans' sound (ADR-0135): MPEG audio (Layer III, and I and II)
//! decoded by `nanomp3-core` (a safe port of minimp3) and converted to what
//! the audio service plays (48 kHz, 16-bit, stereo) by linear
//! interpolation.
//!
//! Made for streaming, from a file or the network alike: a [`Stream`] takes
//! the bytes in pieces of any size ([`Stream::push`]) and gives output
//! frames ([`Stream::render`]) as far as what it holds goes. A frame is
//! decoded only once the input holds [`LOOKAHEAD`] bytes (or the input has
//! ended): the decoder recognises a frame by the ones after it, and would
//! take one cut short for junk.
//!
//! The input is untrusted: anything that is not a frame is skipped, and a
//! corrupt frame is skipped and counted.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use nanomp3_core::{DecodeError, Decoder, MAX_SAMPLES_PER_FRAME};
pub use oceans_wav::{OUTPUT_FRAME, OUTPUT_RATE};

/// Bytes of input held.
pub const INPUT: usize = 32 * 1024;
/// Bytes held before a frame is decoded (many frames: the largest MP3
/// frame is 1441 bytes).
pub const LOOKAHEAD: usize = 16 * 1024;

/// What is being decoded, from the last frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: u8,
    /// Kilobits a second (0: free format).
    pub bitrate: u32,
    /// 1, 2 or 3.
    pub layer: u8,
}

impl Format {
    /// About how many milliseconds `bytes` of this stream play (exact for
    /// a constant bitrate).
    pub fn duration_ms(&self, bytes: u64) -> u64 {
        bytes.saturating_mul(8) / u64::from(self.bitrate.max(1))
    }
}

pub struct Stream {
    /// On the heap, like the buffers: about 60 KiB in all, too much for
    /// a program's 64 KiB stack.
    decoder: Box<Decoder>,
    input: Vec<u8>,
    filled: usize,
    ended: bool,
    /// The last decoded frame's samples (interleaved), its source frames
    /// and channels.
    pcm: Vec<i16>,
    frames: usize,
    channels: usize,
    /// The source frame before `pcm`'s first, interpolated from.
    previous: [i32; 2],
    /// Where the next output frame falls, in 1/2^32ths of a source frame:
    /// 0 is `previous`, 1 `pcm`'s first frame.
    position: u64,
    /// Source frames per output frame, in 1/2^32ths.
    step: u64,
    format: Option<Format>,
    corrupt: u32,
}

impl Stream {
    /// A stream (its decoder and buffers on the heap; the stream itself
    /// too, for the caller to keep).
    pub fn new() -> Box<Self> {
        Box::new(Self {
            decoder: Box::new(Decoder::new()),
            input: vec![0; INPUT],
            filled: 0,
            ended: false,
            pcm: vec![0; MAX_SAMPLES_PER_FRAME],
            frames: 0,
            channels: 2,
            previous: [0; 2],
            position: 1 << 32,
            step: 1 << 32,
            format: None,
            corrupt: 0,
        })
    }

    /// Back to the start, for input from elsewhere (a seek): what is held
    /// is dropped, and the decoder starts over.
    pub fn reset(&mut self) {
        *self.decoder = Decoder::new();
        self.filled = 0;
        self.ended = false;
        self.frames = 0;
        self.previous = [0; 2];
        self.position = 1 << 32;
    }

    /// The format of the last frame decoded.
    pub fn format(&self) -> Option<Format> {
        self.format
    }

    /// Corrupt frames skipped so far.
    pub fn corrupt(&self) -> u32 {
        self.corrupt
    }

    /// Room for more input, in bytes.
    pub fn room(&self) -> usize {
        INPUT - self.filled
    }

    /// Whether more input would let it decode (it holds less than
    /// [`LOOKAHEAD`]).
    pub fn hungry(&self) -> bool {
        !self.ended && self.filled < LOOKAHEAD
    }

    /// Takes what fits of `bytes`; returns how much.
    pub fn push(&mut self, bytes: &[u8]) -> usize {
        let len = bytes.len().min(self.room());
        self.input[self.filled..self.filled + len].copy_from_slice(&bytes[..len]);
        self.filled += len;
        len
    }

    /// The free part of the input, to read into, then [`Stream::pushed`].
    pub fn space(&mut self) -> &mut [u8] {
        &mut self.input[self.filled..]
    }

    /// `len` bytes were read into [`Stream::space`].
    pub fn pushed(&mut self, len: usize) {
        self.filled = (self.filled + len).min(INPUT);
    }

    /// No more input: what is held is decoded to its end.
    pub fn end(&mut self) {
        self.ended = true;
    }

    /// Whether the input has ended and everything has been rendered.
    pub fn finished(&self) -> bool {
        self.ended && self.filled == 0 && (self.position >> 32) as usize >= self.frames
    }

    /// Source frame `index` (0: `previous`), channel `channel`.
    fn sample(&self, index: usize, channel: usize) -> i32 {
        if index == 0 {
            return self.previous[channel];
        }
        let at = (index - 1) * self.channels + channel.min(self.channels - 1);
        i32::from(self.pcm[at])
    }

    /// Decodes the next frame into `pcm`. False: not enough input yet (or
    /// none left).
    fn decode(&mut self) -> bool {
        loop {
            if self.filled == 0 || (self.filled < LOOKAHEAD && !self.ended) {
                return false;
            }
            let (used, result) = self
                .decoder
                .decode(&self.input[..self.filled], &mut self.pcm[..]);
            let used = used.min(self.filled);
            self.input.copy_within(used..self.filled, 0);
            self.filled -= used;
            match result {
                Ok(info) if info.samples_produced > 0 => {
                    let channels = usize::from(info.channels.num());
                    if !(8_000..=48_000).contains(&info.sample_rate) {
                        self.corrupt += 1;
                        continue;
                    }
                    self.frames = info.samples_produced;
                    self.channels = channels;
                    self.step = (u64::from(info.sample_rate) << 32) / u64::from(OUTPUT_RATE);
                    self.format = Some(Format {
                        rate: info.sample_rate,
                        channels: info.channels.num(),
                        bitrate: info.bitrate,
                        layer: info.layer,
                    });
                    return true;
                }
                Ok(_) | Err(DecodeError::ReservoirUnavailable(_)) => {}
                Err(DecodeError::NoFrame) => {
                    if used == 0 {
                        // Nothing it can use: at the end, that is all.
                        if self.ended {
                            self.filled = 0;
                        }
                        return false;
                    }
                }
                Err(_) => self.corrupt += 1,
            }
        }
    }

    /// Output frames into `out` (whole output frames), as far as the input
    /// goes. Returns the frames written.
    pub fn render(&mut self, out: &mut [u8]) -> usize {
        let wanted = out.len() / OUTPUT_FRAME;
        let mut written = 0;
        while written < wanted {
            let index = (self.position >> 32) as usize;
            if index >= self.frames {
                // Past the frame: its last becomes `previous`, and the
                // next is decoded.
                if self.frames > 0 {
                    self.previous = [self.sample(self.frames, 0), self.sample(self.frames, 1)];
                    self.position -= (self.frames as u64) << 32;
                    self.frames = 0;
                    continue;
                }
                if !self.decode() {
                    break;
                }
                continue;
            }
            let fraction = (self.position & 0xffff_ffff) as i64;
            for channel in 0..2 {
                let a = i64::from(self.sample(index, channel));
                let b = i64::from(self.sample(index + 1, channel));
                let value = (a + (((b - a) * fraction) >> 32)) as i16;
                let at = written * OUTPUT_FRAME + channel * 2;
                out[at..at + 2].copy_from_slice(&value.to_le_bytes());
            }
            self.position += self.step;
            written += 1;
        }
        written
    }
}

#[cfg(test)]
mod tests;
