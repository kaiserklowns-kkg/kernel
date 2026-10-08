//! WAV files for Oceans' sound (ADR-0079, ADR-0094): the header, and the
//! samples converted to what the audio service plays (48 kHz, 16-bit,
//! stereo) by linear interpolation.
//!
//! Made for streaming: [`Wav::parse`] needs only the start of a file (its
//! header), and [`Wav::render`] converts any run of output frames from the
//! source frames [`Wav::source_range`] names, so a player reads a song a
//! piece at a time instead of holding it whole.

#![no_std]

use core::ops::RangeInclusive;

/// The rate the audio service plays.
pub const OUTPUT_RATE: u32 = 48_000;
/// Bytes of an output frame: two 16-bit samples.
pub const OUTPUT_FRAME: usize = 4;

/// `fmt ` format tags: integer PCM, and the extensible form PCM may be
/// written in.
const PCM: u16 = 1;
const EXTENSIBLE: u16 = 0xfffe;

/// A WAV file's sound: its format and where its samples are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wav {
    pub channels: u16,
    pub rate: u32,
    /// Byte offset of the samples in the file.
    pub data_offset: u64,
    /// Bytes of samples (whole frames).
    pub data_len: u64,
}

impl Wav {
    /// Reads the header from `start`, the first bytes of the file (the
    /// format and the start of the `data` chunk must be in it; 4 KiB is
    /// plenty for what writers make). `file_len` bounds the samples.
    pub fn parse(start: &[u8], file_len: u64) -> Result<Self, &'static str> {
        if start.len() < 12 || &start[..4] != b"RIFF" || &start[8..12] != b"WAVE" {
            return Err("not a WAV file");
        }
        let mut at = 12;
        let mut format = None;
        while at + 8 <= start.len() {
            let id = &start[at..at + 4];
            let size =
                u32::from_le_bytes([start[at + 4], start[at + 5], start[at + 6], start[at + 7]])
                    as usize;
            let body = &start[at + 8..(at + 8).saturating_add(size).min(start.len())];
            match id {
                b"fmt " if body.len() >= 16 => {
                    let word = |i: usize| u16::from_le_bytes([body[i], body[i + 1]]);
                    let (kind, channels, bits) = (word(0), word(2), word(14));
                    let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
                    if !matches!(kind, PCM | EXTENSIBLE) || bits != 16 {
                        return Err("only 16-bit PCM WAV files are played");
                    }
                    if !(1..=2).contains(&channels) || !(8_000..=192_000).contains(&rate) {
                        return Err("only mono or stereo, 8 to 192 kHz");
                    }
                    format = Some((channels, rate));
                }
                b"data" => {
                    let (channels, rate) = format.ok_or("the data comes before the format")?;
                    let data_offset = (at + 8) as u64;
                    let frame = u64::from(channels) * 2;
                    // A length past the end (a file cut short, or a writer
                    // that never filled it in) stops at the end.
                    let data_len = (size as u64).min(file_len.saturating_sub(data_offset));
                    return Ok(Self {
                        channels,
                        rate,
                        data_offset,
                        data_len: data_len / frame * frame,
                    });
                }
                _ => {}
            }
            // Chunks are padded to an even size.
            at = at + 8 + size + (size & 1);
        }
        Err("no sound data in the file")
    }

    /// Bytes of a source frame.
    pub fn frame(&self) -> usize {
        usize::from(self.channels) * 2
    }

    /// Frames in the file.
    pub fn frames(&self) -> u64 {
        self.data_len / self.frame() as u64
    }

    /// Frames once converted to [`OUTPUT_RATE`].
    pub fn output_frames(&self) -> u64 {
        self.frames() * u64::from(OUTPUT_RATE) / u64::from(self.rate)
    }

    /// The length in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.frames() * 1000 / u64::from(self.rate)
    }

    /// Where output frame `frame` falls in the file, in 1/65536ths of a
    /// source frame.
    fn position(&self, frame: u64) -> u64 {
        ((frame * u64::from(self.rate)) << 16) / u64::from(OUTPUT_RATE)
    }

    /// The source frames output frames `first..first + count` are made
    /// from (each interpolates between two), clamped to the file.
    pub fn source_range(&self, first: u64, count: usize) -> RangeInclusive<u64> {
        let last_frame = self.frames().saturating_sub(1);
        let start = (self.position(first) >> 16).min(last_frame);
        let end = (self.position(first + count.max(1) as u64 - 1) >> 16) + 1;
        start..=end.min(last_frame)
    }

    /// Converts output frames from `first` into `out` (whole output
    /// frames), from `source`: the bytes of source frames from
    /// `source_first` on, as [`Wav::source_range`] named. Returns the
    /// frames written.
    pub fn render(&self, source: &[u8], source_first: u64, first: u64, out: &mut [u8]) -> usize {
        let frame = self.frame();
        let channels = usize::from(self.channels);
        let available = (source.len() / frame) as u64;
        if available == 0 {
            return 0;
        }
        let last = source_first + available - 1;
        let sample = |index: u64, channel: usize| -> i32 {
            let index = (index.clamp(source_first, last) - source_first) as usize;
            let at = index * frame + channel.min(channels - 1) * 2;
            i32::from(i16::from_le_bytes([source[at], source[at + 1]]))
        };
        let count =
            (out.len() / OUTPUT_FRAME).min(self.output_frames().saturating_sub(first) as usize);
        for i in 0..count {
            let position = self.position(first + i as u64);
            let (index, fraction) = (position >> 16, (position & 0xffff) as i32);
            for channel in 0..2 {
                let (a, b) = (sample(index, channel), sample(index + 1, channel));
                let value = (a + (((b - a) * fraction) >> 16)) as i16;
                let at = i * OUTPUT_FRAME + channel * 2;
                out[at..at + 2].copy_from_slice(&value.to_le_bytes());
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    /// A WAV file: a `fmt ` chunk (`kind`), an odd-sized chunk to skip,
    /// and the samples.
    fn file(kind: u16, channels: u16, rate: u32, samples: &[i16]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"LIST\x03\0\0\0abc\0");
        out.extend_from_slice(b"data");
        out.extend_from_slice(&((samples.len() * 2) as u32).to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    fn render_all(wav: &Wav, bytes: &[u8]) -> Vec<i16> {
        let data = &bytes[wav.data_offset as usize..];
        let mut out = std::vec![0u8; wav.output_frames() as usize * OUTPUT_FRAME];
        let written = wav.render(data, 0, 0, &mut out);
        assert_eq!(written as u64, wav.output_frames());
        out.chunks(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect()
    }

    #[test]
    fn the_header() {
        let bytes = file(PCM, 2, 44_100, &[1, 2, 3, 4, 5, 6]);
        let wav = Wav::parse(&bytes, bytes.len() as u64).unwrap();
        assert_eq!((wav.channels, wav.rate), (2, 44_100));
        assert_eq!(wav.data_offset, 56);
        assert_eq!((wav.data_len, wav.frames()), (12, 3));
        // Extensible PCM is PCM.
        let bytes = file(EXTENSIBLE, 1, 48_000, &[0; 480]);
        let wav = Wav::parse(&bytes, bytes.len() as u64).unwrap();
        assert_eq!((wav.frames(), wav.duration_ms()), (480, 10));
    }

    #[test]
    fn what_is_refused() {
        assert_eq!(Wav::parse(b"RIFX", 4), Err("not a WAV file"));
        let mut bytes = file(3, 2, 44_100, &[0; 4]);
        assert_eq!(
            Wav::parse(&bytes, bytes.len() as u64),
            Err("only 16-bit PCM WAV files are played")
        );
        bytes = file(PCM, 6, 44_100, &[0; 4]);
        assert_eq!(
            Wav::parse(&bytes, bytes.len() as u64),
            Err("only mono or stereo, 8 to 192 kHz")
        );
        bytes = file(PCM, 2, 44_100, &[]);
        bytes.truncate(36);
        assert_eq!(
            Wav::parse(&bytes, bytes.len() as u64),
            Err("no sound data in the file")
        );
    }

    #[test]
    fn a_file_cut_short_stops_at_its_end() {
        let bytes = file(PCM, 2, 48_000, &[7; 8]);
        // Two frames and half a third are there.
        let wav = Wav::parse(&bytes, 56 + 10).unwrap();
        assert_eq!(wav.frames(), 2);
    }

    #[test]
    fn same_rate_stereo_is_unchanged() {
        let samples = [100, -100, 200, -200, 300, -300];
        let bytes = file(PCM, 2, 48_000, &samples);
        let wav = Wav::parse(&bytes, bytes.len() as u64).unwrap();
        assert_eq!(render_all(&wav, &bytes), samples);
    }

    #[test]
    fn mono_goes_to_both_channels_and_rates_are_interpolated() {
        // 24 kHz: each source frame becomes two, the second halfway.
        let bytes = file(PCM, 1, 24_000, &[0, 1000, 2000]);
        let wav = Wav::parse(&bytes, bytes.len() as u64).unwrap();
        assert_eq!(wav.output_frames(), 6);
        assert_eq!(
            render_all(&wav, &bytes),
            [
                0, 0, 500, 500, 1000, 1000, 1500, 1500, 2000, 2000, 2000, 2000
            ]
        );
    }

    #[test]
    fn rendering_in_pieces_is_rendering_whole() {
        let samples: Vec<i16> = (0..2000).map(|i| ((i * 37) % 4001 - 2000) as i16).collect();
        let bytes = file(PCM, 2, 44_100, &samples);
        let wav = Wav::parse(&bytes, bytes.len() as u64).unwrap();
        let whole = render_all(&wav, &bytes);
        let data = &bytes[wav.data_offset as usize..];
        let mut pieces = Vec::new();
        let mut first = 0;
        while first < wav.output_frames() {
            let count = 37;
            let range = wav.source_range(first, count);
            let (from, to) = (*range.start() as usize, *range.end() as usize);
            let source = &data[from * wav.frame()..(to + 1) * wav.frame()];
            let mut out = [0u8; 37 * OUTPUT_FRAME];
            let written = wav.render(source, from as u64, first, &mut out);
            pieces.extend(
                out[..written * OUTPUT_FRAME]
                    .chunks(2)
                    .map(|b| i16::from_le_bytes([b[0], b[1]])),
            );
            first += written as u64;
        }
        assert_eq!(pieces, whole);
    }
}
