//! `play`: sound through the audio service (ADR-0079, ADR-0087).
//!
//! ```text
//! play info                   the sound device, its output and its input
//! play tone [HZ] [SECONDS]    a sine tone (440 Hz, 1 s)
//! play wav PATH               a WAV file: 16-bit PCM, mono or stereo, any
//!                             rate (converted to 48 kHz stereo)
//! play record PATH [SECONDS]  record from the input (5 s) into a WAV file,
//!                             48 kHz 16-bit stereo
//! ```
//!
//! Needs `use:audio` (`run play out use:audio -- tone 440 2`); `wav` and
//! `record` also need `use:fs`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt::Write;

use oceans_audio_proto::{CHANNELS, FRAME, Input, Output, SAMPLE_RATE};
use oceans_fs_proto::{Kind, Node, flags};
use oceans_rt::{Directory, Out, Start};
use oceans_wav::Wav;
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\n");
oceans_rt::entry!(main);

const USAGE: &str = "usage: play info | tone [HZ] [SECONDS] | wav PATH | record PATH [SECONDS]";
/// The session buffer: a sixth of a second at a time.
const CHUNK: usize = 32 * 1024;
/// The largest WAV file read.
const MAX_WAV: u64 = 32 << 20;

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let audio = match require(&mut out, &directory, "play", "use", "audio", "use:audio") {
        Ok(audio) => audio,
        Err(code) => return code,
    };
    let args = directory.args();
    let words: Vec<&str> = args.split_whitespace().collect();
    let result = match words.as_slice() {
        ["info"] | [] => {
            let mut text = [0u8; 160];
            oceans_audio_proto::info(audio, &mut text).map(|len| {
                let _ = writeln!(
                    out,
                    "play: {}",
                    core::str::from_utf8(&text[..len]).unwrap_or("?")
                );
                match oceans_audio_proto::input_info(audio, &mut text) {
                    Ok(len) => {
                        let input = core::str::from_utf8(&text[..len]).unwrap_or("?");
                        let _ = writeln!(out, "play: input: {input}");
                    }
                    Err(_) => {
                        let _ = writeln!(out, "play: no input");
                    }
                }
            })
        }
        ["tone", rest @ ..] if rest.len() <= 2 => {
            let hz = rest.first().map_or(Some(440), |w| w.parse().ok());
            let seconds = rest.get(1).map_or(Some(1), |w| w.parse().ok());
            match (hz, seconds) {
                (Some(hz @ 20..=20_000), Some(seconds @ 1..=60)) => {
                    let _ = writeln!(out, "play: a {hz} Hz tone for {seconds} s");
                    let _ = out.flush();
                    tone(audio, hz, seconds)
                }
                _ => {
                    let _ = writeln!(out, "play: 20 to 20000 Hz, 1 to 60 s");
                    return EXIT_USAGE;
                }
            }
        }
        ["wav", path] => return wav(&mut out, &directory, audio, path),
        ["record", path, rest @ ..] if rest.len() <= 1 => {
            match rest.first().map_or(Some(5), |w| w.parse().ok()) {
                Some(seconds @ 1..=60) => {
                    return record(&mut out, &directory, audio, path, seconds);
                }
                _ => {
                    let _ = writeln!(out, "play: 1 to 60 s");
                    return EXIT_USAGE;
                }
            }
        }
        _ => {
            let _ = writeln!(out, "{USAGE}");
            return EXIT_USAGE;
        }
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(out, "play: {}", error.message());
            EXIT_FAILED
        }
    }
}

/// sin(x) for x in turns (0..1 is a whole cycle), close enough for a
/// tone (Bhaskara's approximation).
fn sine(turns: f32) -> f32 {
    const PI: f32 = core::f32::consts::PI;
    let x = (turns - (turns as u32) as f32) * 2.0 * PI;
    let (x, sign) = if x > PI { (x - PI, -1.0) } else { (x, 1.0) };
    sign * 16.0 * x * (PI - x) / (5.0 * PI * PI - 4.0 * x * (PI - x))
}

fn tone(
    audio: oceans_rt::Handle,
    hz: u32,
    seconds: u32,
) -> Result<(), oceans_audio_proto::AudioError> {
    let mut output = Output::open(audio, CHUNK)?;
    let frames = u64::from(SAMPLE_RATE) * u64::from(seconds);
    let mut frame = 0u64;
    while frame < frames {
        let count = ((frames - frame) as usize).min(CHUNK / FRAME);
        let buffer = output.buffer();
        for i in 0..count {
            let n = frame + i as u64;
            let turns = (n * u64::from(hz) % u64::from(SAMPLE_RATE)) as f32 / SAMPLE_RATE as f32;
            // Half of full scale; a few milliseconds of fade at both ends.
            let edge = n.min(frames - 1 - n).min(240) as f32 / 240.0;
            let sample = (sine(turns) * 16_000.0 * edge) as i16;
            let bytes = sample.to_le_bytes();
            buffer[i * FRAME..i * FRAME + 2].copy_from_slice(&bytes);
            buffer[i * FRAME + 2..i * FRAME + 4].copy_from_slice(&bytes);
        }
        output.play(0, (count * FRAME) as u32)?;
        frame += count as u64;
    }
    output.drain()
}

fn wav(out: &mut Out, directory: &Directory, audio: oceans_rt::Handle, path: &str) -> i64 {
    let fs = match require(out, directory, "play", "use", "fs", "use:fs") {
        Ok(fs) => Node(fs),
        Err(code) => return code,
    };
    let bytes = match read_file(&fs, path) {
        Ok(bytes) => bytes,
        Err(why) => {
            let _ = writeln!(out, "play: {path}: {why}");
            return EXIT_FAILED;
        }
    };
    let wav = match Wav::parse(&bytes, bytes.len() as u64) {
        Ok(wav) => wav,
        Err(why) => {
            let _ = writeln!(out, "play: {path}: {why}");
            return EXIT_FAILED;
        }
    };
    let tenths = wav.duration_ms() / 100;
    let _ = writeln!(
        out,
        "play: {path}: {} Hz, {}, {}.{} s",
        wav.rate,
        if wav.channels == 1 { "mono" } else { "stereo" },
        tenths / 10,
        tenths % 10
    );
    let _ = out.flush();
    let start = wav.data_offset as usize;
    let data = &bytes[start..start + wav.data_len as usize];
    match play_wav(audio, &wav, data) {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(out, "play: {}", error.message());
            EXIT_FAILED
        }
    }
}

/// The file's frames, converted to 48 kHz stereo (`oceans-wav`).
fn play_wav(
    audio: oceans_rt::Handle,
    wav: &Wav,
    data: &[u8],
) -> Result<(), oceans_audio_proto::AudioError> {
    let mut output = Output::open(audio, CHUNK)?;
    let mut done = 0;
    while done < wav.output_frames() {
        let written = wav.render(data, 0, done, output.buffer());
        if written == 0 {
            break;
        }
        output.play(0, (written * FRAME) as u32)?;
        done += written as u64;
    }
    output.drain()
}

/// `record`: what comes in for `seconds`, into a new WAV file at `path`.
fn record(
    out: &mut Out,
    directory: &Directory,
    audio: oceans_rt::Handle,
    path: &str,
    seconds: u32,
) -> i64 {
    let fs = match require(out, directory, "play", "use", "fs", "use:fs") {
        Ok(fs) => Node(fs),
        Err(code) => return code,
    };
    let mut text = [0u8; 160];
    let input = match oceans_audio_proto::input_info(audio, &mut text) {
        Ok(len) => core::str::from_utf8(&text[..len]).unwrap_or("?"),
        Err(_) => {
            let _ = writeln!(out, "play: this machine has no sound input");
            return EXIT_FAILED;
        }
    };
    let file = match fs.walk(path, flags::CREATE_FILE | flags::WRITE) {
        Ok((file, Kind::File)) => file,
        Ok((other, _)) => {
            other.close();
            let _ = writeln!(out, "play: {path}: a directory");
            return EXIT_FAILED;
        }
        Err(error) => {
            let _ = writeln!(out, "play: {path}: {}", error.message());
            return EXIT_FAILED;
        }
    };
    let _ = writeln!(out, "play: recording {seconds} s from {input}");
    let _ = out.flush();
    let bytes = u64::from(SAMPLE_RATE) * FRAME as u64 * u64::from(seconds);
    let result = (|| -> Result<u16, &'static str> {
        file.truncate(0).map_err(|e| e.message())?;
        file.write_all(0, &wav_header(bytes as u32))
            .map_err(|e| e.message())?;
        let input = Input::open(audio, CHUNK).map_err(|e| e.message())?;
        let (mut done, mut peak) = (0u64, 0u16);
        while done < bytes {
            let len = (bytes - done).min(CHUNK as u64) as usize;
            input.record(0, len as u32).map_err(|e| e.message())?;
            let samples = &input.buffer()[..len];
            for &sample in samples.as_chunks::<2>().0 {
                peak = peak.max(i16::from_le_bytes(sample).unsigned_abs());
            }
            file.write_all(WAV_HEADER as u64 + done, samples)
                .map_err(|e| e.message())?;
            done += len as u64;
        }
        let _ = input.stop();
        Ok(peak)
    })();
    // Closing commits the file (ADR-0022).
    file.close();
    match result {
        Ok(peak) => {
            let _ = writeln!(
                out,
                "play: recorded {bytes} bytes ({seconds} s) into {path}, peak {}%",
                u32::from(peak) * 100 / 32_768
            );
            0
        }
        Err(why) => {
            let _ = writeln!(out, "play: {path}: {why}");
            EXIT_FAILED
        }
    }
}

const WAV_HEADER: usize = 44;

/// The header of a 48 kHz 16-bit stereo PCM WAV file of `data` bytes.
fn wav_header(data: u32) -> [u8; WAV_HEADER] {
    let mut header = [0u8; WAV_HEADER];
    let mut put = |at: usize, bytes: &[u8]| header[at..at + bytes.len()].copy_from_slice(bytes);
    put(0, b"RIFF");
    put(4, &(36 + data).to_le_bytes());
    put(8, b"WAVEfmt ");
    put(16, &16u32.to_le_bytes());
    put(20, &1u16.to_le_bytes());
    put(22, &(CHANNELS as u16).to_le_bytes());
    put(24, &SAMPLE_RATE.to_le_bytes());
    put(28, &(SAMPLE_RATE * FRAME as u32).to_le_bytes());
    put(32, &(FRAME as u16).to_le_bytes());
    put(34, &16u16.to_le_bytes());
    put(36, b"data");
    put(40, &data.to_le_bytes());
    header
}

fn read_file(fs: &Node, path: &str) -> Result<Vec<u8>, &'static str> {
    let (file, kind) = fs.walk(path, 0).map_err(|_| "not found")?;
    let result = (|| {
        if kind != Kind::File {
            return Err("not a file");
        }
        let size = file.stat().map_err(|_| "cannot read it")?.size;
        if size > MAX_WAV {
            return Err("larger than 32 MiB");
        }
        let mut bytes = alloc::vec![0u8; size as usize];
        let mut done = 0;
        while done < bytes.len() {
            match file.read(done as u64, &mut bytes[done..]) {
                Ok(0) | Err(_) => return Err("cannot read it"),
                Ok(got) => done += got,
            }
        }
        Ok(bytes)
    })();
    file.close();
    result
}
