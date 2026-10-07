extern crate std;

use std::vec;
use std::vec::Vec;

use super::*;

#[path = "fixtures.rs"]
mod fixtures;

// ---- A small encoder, for exact round trips ------------------------------

/// A zlib stream of stored blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = if data.is_empty() {
        vec![&[][..]]
    } else {
        data.chunks(65_535).collect()
    };
    for (i, block) in blocks.iter().enumerate() {
        out.push(u8::from(i + 1 == blocks.len()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&inflate::adler32(data).to_be_bytes());
    out
}

fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = (body.len() as u32).to_be_bytes().to_vec();
    let mut typed = kind.to_vec();
    typed.extend_from_slice(body);
    out.extend_from_slice(&typed);
    out.extend_from_slice(&png::crc32(&typed).to_be_bytes());
    out
}

fn ihdr(width: u32, height: u32, depth: u8, colour: u8, interlaced: bool) -> Vec<u8> {
    let mut body = width.to_be_bytes().to_vec();
    body.extend_from_slice(&height.to_be_bytes());
    body.extend_from_slice(&[depth, colour, 0, 0, u8::from(interlaced)]);
    chunk(b"IHDR", &body)
}

fn paeth_predict(a: u8, b: u8, c: u8) -> u8 {
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

/// Filters `rows` (unfiltered bytes, `stride` each), row `i` with filter
/// `i % 5`, as an encoder would.
fn filter_rows(rows: &[Vec<u8>], step: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for (y, row) in rows.iter().enumerate() {
        let kind = (y % 5) as u8;
        out.push(kind);
        let empty = vec![0u8; row.len()];
        let prev = if y > 0 { &rows[y - 1] } else { &empty };
        for i in 0..row.len() {
            let left = if i >= step { row[i - step] } else { 0 };
            let up = prev[i];
            let corner = if i >= step { prev[i - step] } else { 0 };
            let predicted = match kind {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                _ => paeth_predict(left, up, corner),
            };
            out.push(row[i].wrapping_sub(predicted));
        }
    }
    out
}

/// A PNG of `rows` (unfiltered), with `extra` chunks before the data.
fn png_file(header: Vec<u8>, extra: &[Vec<u8>], rows: &[Vec<u8>], step: usize) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    out.extend(header);
    for chunk in extra {
        out.extend_from_slice(chunk);
    }
    out.extend(chunk(b"IDAT", &zlib_stored(&filter_rows(rows, step))));
    out.extend(chunk(b"IEND", &[]));
    out
}

/// Packs `width` samples of `depth` bits (≤ 8) per row, most significant
/// first.
fn pack(samples: &[u8], depth: usize) -> Vec<u8> {
    let mut out = vec![0u8; (samples.len() * depth).div_ceil(8)];
    for (i, &s) in samples.iter().enumerate() {
        let bit = i * depth;
        out[bit / 8] |= s << (8 - depth - bit % 8);
    }
    out
}

// ---- The inflater -------------------------------------------------------

#[test]
fn inflates_what_zlib_compressed() {
    assert_eq!(
        inflate::zlib(fixtures::TEXT_ZLIB, fixtures::TEXT.len()).unwrap(),
        fixtures::TEXT
    );
    assert_eq!(
        inflate::zlib(fixtures::SHORT_ZLIB, 9).unwrap(),
        b"abcabcabc"
    );
    // Stored blocks, across the 64 KiB block size.
    let data: Vec<u8> = (0..70_000u32).map(|i| (i * 7 + i / 300) as u8).collect();
    assert_eq!(
        inflate::zlib(&zlib_stored(&data), data.len()).unwrap(),
        data
    );
}

#[test]
fn inflating_refuses_bad_streams() {
    // More output than the limit allows.
    assert!(matches!(
        inflate::zlib(fixtures::TEXT_ZLIB, fixtures::TEXT.len() - 1),
        Err(Error::Corrupt(_))
    ));
    // A changed checksum, a changed body, a cut stream.
    let mut wrong = fixtures::TEXT_ZLIB.to_vec();
    let last = wrong.len() - 1;
    wrong[last] ^= 1;
    assert_eq!(
        inflate::zlib(&wrong, fixtures::TEXT.len()),
        Err(Error::Corrupt("the image data's checksum"))
    );
    let mut flipped = fixtures::TEXT_ZLIB.to_vec();
    flipped[40] ^= 0x10;
    assert!(inflate::zlib(&flipped, fixtures::TEXT.len()).is_err());
    let cut = &fixtures::TEXT_ZLIB[..fixtures::TEXT_ZLIB.len() / 2];
    assert!(inflate::zlib(cut, fixtures::TEXT.len()).is_err());
    // Not zlib; a preset dictionary; a reserved block type.
    assert!(inflate::zlib(&[0x12, 0x34, 0, 0], 10).is_err());
    assert_eq!(
        inflate::zlib(&[0x78, 0xbb, 0, 0, 0, 0], 10),
        Err(Error::Unsupported("a preset zlib dictionary"))
    );
    assert!(inflate::inflate(&[0b111], 10).is_err());
    // A stored block whose length check is wrong.
    assert!(inflate::inflate(&[1, 5, 0, 5, 0, 1, 2, 3, 4, 5], 10).is_err());
    // With fixed codes: literal 'a', then length 3 at distance 1 (fine),
    // or at distance 2 (before the start of the output).
    assert_eq!(inflate::inflate(&fixed_block(1), 10).unwrap().0, b"aaaa");
    assert_eq!(
        inflate::inflate(&fixed_block(2), 10).err(),
        Some(Error::Corrupt("a distance before the start"))
    );
}

/// A final fixed-code block: 'a', a copy of 3 at distance `distance` (1
/// or 2: distance codes 0 and 1), end.
fn fixed_block(distance: u32) -> Vec<u8> {
    let mut bits: Vec<bool> = Vec::new();
    // Header fields go least significant bit first; Huffman codes most
    // significant first.
    let field = |bits: &mut Vec<bool>, value: u32, count: u32| {
        bits.extend((0..count).map(|i| value >> i & 1 == 1));
    };
    let code = |bits: &mut Vec<bool>, value: u32, count: u32| {
        bits.extend((0..count).rev().map(|i| value >> i & 1 == 1));
    };
    field(&mut bits, 1, 1); // BFINAL
    field(&mut bits, 1, 2); // fixed codes
    code(&mut bits, 0x30 + u32::from(b'a'), 8); // literal 'a'
    code(&mut bits, 1, 7); // 257: length 3
    code(&mut bits, distance - 1, 5); // distance code
    code(&mut bits, 0, 7); // 256: end of block
    bits.chunks(8)
        .map(|byte| {
            byte.iter()
                .enumerate()
                .fold(0u8, |b, (i, &on)| b | u8::from(on) << i)
        })
        .collect()
}

// ---- PNG ---------------------------------------------------------------

#[test]
fn decodes_a_png_made_by_zlib() {
    let (format, image) = decode(fixtures::GRADIENT_PNG).unwrap();
    assert_eq!(format, Format::Png);
    assert_eq!((image.width, image.height), (40, 30));
    for y in 0..30u32 {
        for x in 0..40u32 {
            let (r, g, b) = ((x * 6) & 0xff, (y * 8) & 0xff, ((x + y) * 3) & 0xff);
            assert_eq!(
                image.pixels[(y * 40 + x) as usize],
                0xff00_0000 | r << 16 | g << 8 | b,
                "pixel {x},{y}"
            );
        }
    }
}

#[test]
fn decodes_every_colour_type() {
    // RGBA, 8 bits: every filter type.
    let rows: Vec<Vec<u8>> = (0..7u8)
        .map(|y| {
            (0..5u8)
                .flat_map(|x| [x * 40, y * 30, x ^ y, 255 - x * 10])
                .collect()
        })
        .collect();
    let image = decode(&png_file(ihdr(5, 7, 8, 6, false), &[], &rows, 4))
        .unwrap()
        .1;
    assert_eq!(image.pixels[0], 0xff00_0000);
    assert_eq!(
        image.pixels[6 * 5 + 4],
        ((255 - 40) << 24) | (160 << 16) | (180 << 8) | 2
    );

    // Gray, 1 bit: a checkerboard.
    let rows: Vec<Vec<u8>> = (0..3)
        .map(|y| pack(&[y % 2, 1 - y % 2, y % 2], 1))
        .collect();
    let image = decode(&png_file(ihdr(3, 3, 1, 0, false), &[], &rows, 1))
        .unwrap()
        .1;
    assert_eq!(&image.pixels[..3], &[0xff00_0000, 0xffff_ffff, 0xff00_0000]);

    // Palette, 4 bits, with transparency for entry 1.
    let palette = chunk(b"PLTE", &[255, 0, 0, 0, 255, 0, 0, 0, 255]);
    let trns = chunk(b"tRNS", &[255, 0]);
    let rows = vec![pack(&[0, 1, 2], 4)];
    let image = decode(&png_file(
        ihdr(3, 1, 4, 3, false),
        &[palette.clone(), trns],
        &rows,
        1,
    ))
    .unwrap()
    .1;
    assert_eq!(image.pixels, [0xffff_0000, 0x0000_ff00, 0xff00_00ff]);
    // An index beyond the palette.
    let rows = vec![pack(&[0, 5, 2], 4)];
    assert_eq!(
        decode(&png_file(ihdr(3, 1, 4, 3, false), &[palette], &rows, 1)),
        Err(Error::Corrupt("a colour beyond the palette"))
    );

    // RGB, 16 bits, with a transparent colour key.
    let trns = chunk(b"tRNS", &[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
    let rows = vec![vec![
        0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0x12, 0x35, 0x56, 0x78, 0x9a, 0xbc,
    ]];
    let image = decode(&png_file(ihdr(2, 1, 16, 2, false), &[trns], &rows, 6))
        .unwrap()
        .1;
    assert_eq!(image.pixels, [0x0012_569a, 0xff12_569a]);

    // Gray and alpha, 8 bits; gray 16 bits with a key.
    let rows = vec![vec![100, 50, 200, 255]];
    let image = decode(&png_file(ihdr(2, 1, 8, 4, false), &[], &rows, 2))
        .unwrap()
        .1;
    assert_eq!(image.pixels, [0x3264_6464, 0xffc8_c8c8]);
    let trns = chunk(b"tRNS", &[0x01, 0x00]);
    let rows = vec![vec![0x01, 0x00, 0xff, 0xff]];
    let image = decode(&png_file(ihdr(2, 1, 16, 0, false), &[trns], &rows, 2))
        .unwrap()
        .1;
    assert_eq!(image.pixels, [0x0001_0101, 0xffff_ffff]);
}

#[test]
fn decodes_adam7_interlacing() {
    // A 10×9 RGB image, sent in Adam7's seven passes.
    let (w, h) = (10u32, 9u32);
    let colour = |x: u32, y: u32| [(x * 20) as u8, (y * 25) as u8, ((x * y) % 256) as u8];
    let mut raw = Vec::new();
    for &(x0, y0, dx, dy) in &[
        (0, 0, 8, 8),
        (4, 0, 8, 8),
        (0, 4, 4, 8),
        (2, 0, 4, 4),
        (0, 2, 2, 4),
        (1, 0, 2, 2),
        (0, 1, 1, 2),
    ] {
        let rows: Vec<Vec<u8>> = (y0..h)
            .step_by(dy as usize)
            .map(|y| {
                (x0..w)
                    .step_by(dx as usize)
                    .flat_map(|x| colour(x, y))
                    .collect()
            })
            .collect();
        if rows.is_empty() || rows[0].is_empty() {
            continue;
        }
        raw.extend(filter_rows(&rows, 3));
    }
    let mut file = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    file.extend(ihdr(w, h, 8, 2, true));
    // The data split over two IDAT chunks.
    let data = zlib_stored(&raw);
    file.extend(chunk(b"IDAT", &data[..data.len() / 2]));
    file.extend(chunk(b"IDAT", &data[data.len() / 2..]));
    file.extend(chunk(b"IEND", &[]));
    let image = decode(&file).unwrap().1;
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = colour(x, y);
            assert_eq!(
                image.pixels[(y * w + x) as usize],
                0xff00_0000 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
                "pixel {x},{y}"
            );
        }
    }
}

#[test]
fn refuses_broken_pngs() {
    let rows = vec![vec![1, 2, 3]];
    let good = png_file(ihdr(1, 1, 8, 2, false), &[], &rows, 3);
    assert!(decode(&good).is_ok());
    // A chunk's checksum.
    let mut bad = good.clone();
    bad[30] ^= 1;
    assert_eq!(decode(&bad), Err(Error::Corrupt("a chunk's checksum")));
    // Cut anywhere.
    for len in [8, 20, 33, good.len() - 5] {
        assert!(decode(&good[..len]).is_err(), "cut at {len}");
    }
    // Too large, empty, a bad depth, an unknown critical chunk, no IDAT.
    let huge = png_file(ihdr(20_000, 1, 8, 2, false), &[], &rows, 3);
    assert_eq!(decode(&huge), Err(Error::TooLarge));
    let many = png_file(ihdr(4096, 4096, 8, 2, false), &[], &rows, 3);
    assert_eq!(decode(&many), Err(Error::TooLarge));
    assert!(decode(&png_file(ihdr(0, 1, 8, 2, false), &[], &rows, 3)).is_err());
    assert!(decode(&png_file(ihdr(1, 1, 4, 2, false), &[], &rows, 3)).is_err());
    let unknown = chunk(b"ZZZZ", &[]);
    assert_eq!(
        decode(&png_file(ihdr(1, 1, 8, 2, false), &[unknown], &rows, 3)),
        Err(Error::Unsupported("an unknown critical chunk"))
    );
    // Ancillary chunks it does not know are skipped.
    let note = chunk(b"zzzz", &[1, 2, 3]);
    assert!(decode(&png_file(ihdr(1, 1, 8, 2, false), &[note], &rows, 3)).is_ok());
    // Too little image data for its size.
    let short = png_file(ihdr(2, 1, 8, 2, false), &[], &rows, 3);
    assert!(decode(&short).is_err());
}

// ---- BMP ---------------------------------------------------------------

/// A BMP with a 40-byte header (`header` overrides: 56 for masks inside).
fn bmp_file(
    width: i32,
    height: i32,
    bpp: u16,
    compression: u32,
    extra: &[u8],
    data: &[u8],
) -> Vec<u8> {
    let offset = 14 + 40 + extra.len() as u32;
    let mut out = b"BM".to_vec();
    out.extend_from_slice(&(offset + data.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&bpp.to_le_bytes());
    out.extend_from_slice(&compression.to_le_bytes());
    out.extend_from_slice(&[0; 20]);
    out.extend_from_slice(extra);
    out.extend_from_slice(data);
    out
}

#[test]
fn decodes_bitmaps() {
    // 24 bits, bottom-up, rows padded to 4 bytes: 2×2.
    let data = [
        0, 0, 255, 0, 255, 0, 0, 0, // bottom row: red, green, padding
        255, 0, 0, 255, 255, 255, 0, 0, // top row: blue, white
    ];
    let (format, image) = decode(&bmp_file(2, 2, 24, 0, &[], &data)).unwrap();
    assert_eq!(format, Format::Bmp);
    assert_eq!(
        image.pixels,
        [0xff00_00ff, 0xffff_ffff, 0xffff_0000, 0xff00_ff00]
    );

    // 32 bits, top-down, bit fields with alpha (BI_ALPHABITFIELDS).
    let mut masks = Vec::new();
    for mask in [0x00ff_0000u32, 0x0000_ff00, 0x0000_00ff, 0xff00_0000] {
        masks.extend_from_slice(&mask.to_le_bytes());
    }
    let image = decode(&bmp_file(
        1,
        -2,
        32,
        6,
        &masks,
        &[1, 2, 3, 0x80, 4, 5, 6, 0xff],
    ))
    .unwrap()
    .1;
    assert_eq!(image.pixels, [0x8003_0201, 0xff06_0504]);

    // 16 bits, 5-6-5 bit fields.
    let mut masks = Vec::new();
    for mask in [0xf800u32, 0x07e0, 0x001f] {
        masks.extend_from_slice(&mask.to_le_bytes());
    }
    let image = decode(&bmp_file(1, 1, 16, 3, &masks, &[0x1f, 0xf8, 0, 0]))
        .unwrap()
        .1;
    assert_eq!(image.pixels, [0xffff_00ff]);

    // 8 and 1 bits with palettes.
    let palette = [0, 0, 0, 0, 255, 255, 255, 0, 0, 0, 255, 0];
    let image = decode(&bmp_file(3, 1, 8, 0, &palette, &[2, 1, 0, 0]))
        .unwrap()
        .1;
    assert_eq!(image.pixels, [0xffff_0000, 0xffff_ffff, 0xff00_0000]);
    let image = decode(&bmp_file(
        3,
        1,
        1,
        0,
        &palette[..8],
        &[0b0100_0000, 0, 0, 0],
    ))
    .unwrap()
    .1;
    assert_eq!(image.pixels, [0xff00_0000, 0xffff_ffff, 0xff00_0000]);

    // Cut short, compressed, empty, too large.
    assert_eq!(
        decode(&bmp_file(2, 2, 24, 0, &[], &data[..10])),
        Err(Error::Truncated)
    );
    assert!(matches!(
        decode(&bmp_file(2, 2, 8, 1, &[], &data)),
        Err(Error::Unsupported(_))
    ));
    assert!(decode(&bmp_file(0, 2, 24, 0, &[], &data)).is_err());
    assert_eq!(
        decode(&bmp_file(20_000, 2, 24, 0, &[], &data)),
        Err(Error::TooLarge)
    );
    assert_eq!(decode(b"GIF89a"), Err(Error::UnknownFormat));
}

// ---- Showing them -------------------------------------------------------

#[test]
fn fits_scales_and_shows_transparency() {
    assert_eq!(fit(100, 50, 400, 300), (100, 50), "never enlarged");
    assert_eq!(fit(800, 400, 400, 300), (400, 200));
    assert_eq!(fit(400, 800, 400, 300), (150, 300));
    assert_eq!(fit(10_000, 1, 100, 100), (100, 1));

    // 4×2 → 2×1: each output pixel the average of a 2×2 block.
    let image = Image {
        width: 4,
        height: 2,
        pixels: vec![
            0xff00_0000,
            0xff00_0000,
            0xffff_ffff,
            0xffff_ffff,
            0xff00_0000,
            0xff00_0000,
            0xff00_0000,
            0xff00_0000,
        ],
    };
    assert_eq!(scale(&image, 2, 1), [0xff00_0000, 0xff7f_7f7f]);
    // Transparent pixels do not darken the average.
    let half = Image {
        width: 2,
        height: 1,
        pixels: vec![0x0000_0000, 0xffff_0000],
    };
    assert_eq!(scale(&half, 1, 1), [0x7fff_0000]);
    // Growing: nearest pixel.
    assert_eq!(scale(&half, 4, 1), [0, 0, 0xffff_0000, 0xffff_0000]);

    assert_eq!(over_checker(0xff12_3456, 0, 0), 0x0012_3456);
    assert_eq!(over_checker(0x0012_3456, 0, 0), 0x00ff_ffff);
    assert_eq!(over_checker(0x0012_3456, 8, 0), 0x00cc_cccc);
    assert!(is_image_name("Photo.PNG") && is_image_name("a.bmp"));
    assert!(!is_image_name(".png") && !is_image_name("notes.txt"));
}
