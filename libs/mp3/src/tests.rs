use super::*;

/// 2 s of an 880 Hz sine at half scale, 44.1 kHz stereo, 64 kbit/s (made
/// with `ffmpeg -f lavfi -i "aevalsrc=0.5*sin(880*2*PI*t):s=44100:d=2:c=stereo"
/// -c:a libmp3lame -b:a 64k -id3v2_version 0 -write_xing 0`).
const TONE: &[u8] = include_bytes!("../testdata/tone-880.mp3");

/// The whole of `input`, given in pieces of `piece` bytes, rendered in
/// runs of `run` output frames.
fn play(input: &[u8], piece: usize, run: usize) -> (Vec<i16>, Box<Stream>) {
    let mut stream = Stream::new();
    let mut out = vec![0u8; run * OUTPUT_FRAME];
    let mut samples = Vec::new();
    let mut at = 0;
    loop {
        while stream.hungry() && at < input.len() {
            let end = (at + piece).min(input.len());
            at += stream.push(&input[at..end]);
        }
        if at == input.len() {
            stream.end();
        }
        let written = stream.render(&mut out);
        samples.extend(
            out[..written * OUTPUT_FRAME]
                .chunks(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]])),
        );
        if written == 0 && stream.finished() {
            return (samples, stream);
        }
        assert!(written > 0 || stream.hungry(), "stuck");
    }
}

#[test]
fn a_tone_at_48_khz() {
    let (samples, stream) = play(TONE, 4096, 1000);
    let format = stream.format().unwrap();
    assert_eq!(
        (format.rate, format.channels, format.bitrate, format.layer),
        (44_100, 2, 64, 3)
    );
    assert_eq!(stream.corrupt(), 0);
    // About 2 s (the encoder pads the start and the end).
    let frames = samples.len() / 2;
    assert!((96_000..=99_000).contains(&frames), "{frames} frames");
    // 880 Hz: 1760 sign changes a second, in the middle second.
    let left: Vec<i16> = samples.iter().step_by(2).copied().collect();
    let middle = &left[24_000..72_000];
    let crossings = middle
        .windows(2)
        .filter(|w| (w[0] < 0) != (w[1] < 0))
        .count();
    assert!((1750..=1770).contains(&crossings), "{crossings} crossings");
    assert!(middle.iter().any(|&s| s > 15_000), "at half scale");
    // Both channels alike.
    assert!(
        samples
            .chunks(2)
            .all(|f| (i32::from(f[0]) - i32::from(f[1])).abs() < 2)
    );
}

#[test]
fn pieces_of_any_size_give_the_same_sound() {
    let (whole, _) = play(TONE, TONE.len(), 8192);
    for (piece, run) in [(1, 7), (333, 1), (1441, 100), (20_000, 4096)] {
        let (pieces, _) = play(TONE, piece, run);
        assert_eq!(pieces, whole, "pieces of {piece}, runs of {run}");
    }
}

#[test]
fn junk_and_a_cut_are_skipped() {
    // Junk before, a cut in the middle (as when a stream drops bytes).
    let mut input = vec![0x55u8; 5000];
    input.extend_from_slice(&TONE[..6000]);
    input.extend_from_slice(&TONE[6100..]);
    let (samples, _) = play(&input, 1000, 1000);
    assert!(samples.len() / 2 > 90_000, "{} frames", samples.len() / 2);

    // Noise alone: nothing, and no panic.
    let mut seed = 0x1234_5678u32;
    let noise: Vec<u8> = (0..200_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        })
        .collect();
    let _ = play(&noise, 777, 500);
    let (samples, _) = play(&[], 1, 10);
    assert!(samples.is_empty());
}

#[test]
fn reset_starts_over() {
    let mut stream = Stream::new();
    stream.push(TONE);
    stream.end();
    let mut out = [0u8; 400 * OUTPUT_FRAME];
    assert_eq!(stream.render(&mut out), 400);
    stream.reset();
    assert!(stream.hungry());
    assert_eq!(stream.render(&mut out), 0, "nothing held");
    stream.push(&TONE[8000..]);
    stream.end();
    assert_eq!(stream.render(&mut out), 400, "from the middle");
    let format = stream.format().unwrap();
    assert_eq!(format.duration_ms(TONE.len() as u64), 2037);
}
