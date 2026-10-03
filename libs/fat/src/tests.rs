//! Against images made by the reference tools (mkfs.fat, mtools; see
//! `testdata/generate.sh`), and damaged copies of them.

use std::string::{String, ToString};
use std::vec::Vec;
use std::{format, vec};

use super::*;

struct Image(Vec<u8>);

impl Disk for Image {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<(), IoError> {
        let start = usize::try_from(offset).map_err(|_| IoError)?;
        let bytes = self.0.get(start..start + out.len()).ok_or(IoError)?;
        out.copy_from_slice(bytes);
        Ok(())
    }

    fn size(&self) -> u64 {
        self.0.len() as u64
    }
}

/// Expands a `testdata/*.sparse` image (see `sparse.py`).
fn image(name: &str) -> Vec<u8> {
    let blob = std::fs::read(format!(
        "{}/testdata/{name}.sparse",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    assert_eq!(&blob[..8], b"OCSPARSE");
    let size = u64_at(&blob, 8) as usize;
    let mut image = vec![0u8; size];
    let mut at = 16;
    while at < blob.len() {
        let offset = u64_at(&blob, at) as usize;
        let len = u32_at(&blob, at + 8) as usize;
        at += 12;
        image[offset..offset + len].copy_from_slice(&blob[at..at + len]);
        at += len;
    }
    image
}

fn open(name: &str) -> Fat<Image> {
    Fat::open(Image(image(name))).unwrap()
}

fn path(fat: &mut Fat<Image>, path: &str) -> Result<NodeId, Error> {
    let mut node = ROOT;
    for part in path.split('/') {
        node = fat.lookup(node, part)?;
    }
    Ok(node)
}

fn read_all(fat: &mut Fat<Image>, node: NodeId, step: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buffer = vec![0u8; step];
    loop {
        let len = fat.read_file(node, out.len() as u64, &mut buffer).unwrap();
        if len == 0 {
            return out;
        }
        out.extend_from_slice(&buffer[..len]);
    }
}

fn listing(fat: &mut Fat<Image>, dir: NodeId) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for index in 0.. {
        match fat.entry(dir, index) {
            Ok((name, directory)) => out.push((name.to_string(), directory)),
            Err(Error::NotFound) => return out,
            Err(error) => panic!("{error:?}"),
        }
    }
    out
}

const IMAGES: [(&str, FatType, &str); 3] = [
    ("fat12", FatType::Fat12, "OCEANS12"),
    ("fat16", FatType::Fat16, "OCEANS16"),
    ("fat32", FatType::Fat32, "OCEANS32"),
];

#[test]
fn finds_every_layout() {
    for (name, kind, label) in IMAGES {
        let fat = open(name);
        assert_eq!(fat.kind(), kind, "{name}");
        assert_eq!(fat.label(), label);
        assert!(fat.capacity() > 0);
    }
}

#[test]
fn lists_long_short_and_unicode_names() {
    for (name, ..) in IMAGES {
        let mut fat = open(name);
        let entries = listing(&mut fat, ROOT);
        let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "HELLO.TXT",
                "A long file name.txt",
                "long-file-name.txt",
                "ไฟล์ภาษาไทย.txt",
                "empty.txt",
                "Docs",
                "frag.bin",
                "pad2"
            ],
            "{name}"
        );
        assert!(
            entries
                .iter()
                .all(|(n, directory)| *directory == (n == "Docs"))
        );
        let docs = fat.lookup(ROOT, "docs").unwrap();
        assert_eq!(
            listing(&mut fat, docs),
            [("notes".to_string(), true)],
            "no . or .."
        );
    }
}

#[test]
fn reads_files() {
    for (name, ..) in IMAGES {
        let mut fat = open(name);
        let hello = fat.lookup(ROOT, "hello.txt").unwrap();
        assert_eq!(read_all(&mut fat, hello, 7), b"hello from FAT\n");
        let long = fat.lookup(ROOT, "A LONG FILE NAME.TXT").unwrap();
        assert_eq!(read_all(&mut fat, long, 512), b"long names work\n");
        let thai = fat.lookup(ROOT, "ไฟล์ภาษาไทย.txt").unwrap();
        assert_eq!(read_all(&mut fat, thai, 4), "สวัสดี\n".as_bytes());
        let deep = path(&mut fat, "Docs/notes/deep.txt").unwrap();
        assert_eq!(read_all(&mut fat, deep, 100), b"deep\n");
        let empty = fat.lookup(ROOT, "empty.txt").unwrap();
        assert_eq!(fat.size(empty), Ok(0));
        assert_eq!(read_all(&mut fat, empty, 10), b"");
        // Short names reach long-named files too.
        assert!(fat.lookup(ROOT, "ALONGF~1.TXT").is_ok());
    }
}

#[test]
fn reads_fragmented_files_in_any_pieces() {
    let expected: Vec<u8> = (0..20000u32).map(|i| (i % 251) as u8).collect();
    for (name, ..) in IMAGES {
        let mut fat = open(name);
        let frag = fat.lookup(ROOT, "frag.bin").unwrap();
        assert_eq!(fat.size(frag), Ok(20000));
        for step in [1, 333, 512, 4096, 65536] {
            assert!(
                read_all(&mut fat, frag, step) == expected,
                "{name} in pieces of {step}"
            );
        }
        // Backwards: the cluster cursor restarts from the chain's head.
        let mut tail = [0u8; 10];
        assert_eq!(fat.read_file(frag, 19990, &mut tail), Ok(10));
        assert_eq!(&tail[..], &expected[19990..]);
        let mut head = [0u8; 10];
        assert_eq!(fat.read_file(frag, 0, &mut head), Ok(10));
        assert_eq!(&head[..], &expected[..10]);
        assert_eq!(fat.read_file(frag, 20000, &mut head), Ok(0));
        assert_eq!(fat.read_file(frag, 1 << 40, &mut head), Ok(0));
    }
}

#[test]
fn reports_errors() {
    let mut fat = open("fat16");
    assert_eq!(fat.lookup(ROOT, "missing"), Err(Error::NotFound));
    let hello = fat.lookup(ROOT, "HELLO.TXT").unwrap();
    assert_eq!(fat.lookup(hello, "x"), Err(Error::NotADirectory));
    let docs = fat.lookup(ROOT, "Docs").unwrap();
    assert_eq!(
        fat.read_file(docs, 0, &mut [0u8; 4]),
        Err(Error::IsADirectory)
    );
    assert_eq!(fat.size(999), Err(Error::NotFound));
}

#[test]
fn keeps_held_nodes_and_reuses_free_ones() {
    let mut fat = open("fat12");
    let hello = fat.lookup(ROOT, "HELLO.TXT").unwrap();
    fat.retain(hello).unwrap();
    for _ in 0..50 {
        let _ = fat.lookup(ROOT, "Docs").unwrap();
        let _ = fat.lookup(ROOT, "A long file name.txt").unwrap();
    }
    assert!(fat.nodes.len() < 6, "unheld lookups reuse slots");
    assert_eq!(read_all(&mut fat, hello, 64), b"hello from FAT\n");
    fat.release(hello);
}

#[test]
fn refuses_what_is_not_fat() {
    for disk in [vec![0u8; 1 << 20], vec![0xa5u8; 1 << 20], vec![0u8; 100]] {
        assert_eq!(Fat::open(Image(disk)).err(), Some(Error::NotFat));
    }
    // An MBR whose only partition is Linux (0x83).
    let mut mbr = image("fat16");
    mbr[446 + 4] = 0x83;
    assert_eq!(Fat::open(Image(mbr)).err(), Some(Error::NotFat));
    // A boot sector that claims more sectors than the disk has.
    let mut short = image("fat12");
    short.truncate(512 * 1000);
    assert_eq!(Fat::open(Image(short)).err(), Some(Error::NotFat));
    // A broken boot sector: zero sectors per cluster.
    let mut broken = image("fat12");
    broken[13] = 0;
    assert_eq!(Fat::open(Image(broken)).err(), Some(Error::NotFat));
}

#[test]
fn survives_corrupt_chains() {
    // frag.bin's chain in the FAT12 image starts at cluster 9 (see
    // generate.sh: it fills pad1's hole first). A link to a free or bad
    // cluster is corruption.
    for (value, what) in [(0u16, "free"), (0xff7, "bad"), (1, "reserved")] {
        let mut disk = image("fat12");
        // FAT12, first FAT after one reserved sector.
        let fat = 512;
        let at = fat + 9 * 3 / 2;
        let pair = u16::from_le_bytes([disk[at], disk[at + 1]]);
        let pair = (pair & 0x000f) | value << 4; // cluster 9 is odd
        disk[at..at + 2].copy_from_slice(&pair.to_le_bytes());
        let mut fat = Fat::open(Image(disk)).unwrap();
        let frag = fat.lookup(ROOT, "frag.bin").unwrap();
        let mut buffer = vec![0u8; 20000];
        assert_eq!(
            fat.read_file(frag, 0, &mut buffer),
            Err(Error::Corrupt),
            "{what}"
        );
    }
}

#[test]
fn loops_cannot_hang() {
    // Cluster 9 pointing at itself: a file read is bounded by its size.
    let mut disk = image("fat12");
    let at = 512 + 9 * 3 / 2;
    let pair = u16::from_le_bytes([disk[at], disk[at + 1]]);
    disk[at..at + 2].copy_from_slice(&((pair & 0x000f) | 9 << 4).to_le_bytes());
    let mut fat = Fat::open(Image(disk)).unwrap();
    let frag = fat.lookup(ROOT, "frag.bin").unwrap();
    let mut buffer = vec![0u8; 20000];
    assert_eq!(fat.read_file(frag, 0, &mut buffer), Ok(20000));

    // A directory whose cluster points at itself: the walk stops after as
    // many steps as there are clusters.
    let mut disk = image("fat12");
    let mut probe = Fat::open(Image(disk.clone())).unwrap();
    let docs = probe.lookup(ROOT, "Docs").unwrap();
    let cluster = probe.node(docs).unwrap().first as usize;
    let at = 512 + cluster * 3 / 2;
    let pair = u16::from_le_bytes([disk[at], disk[at + 1]]);
    let looped = if cluster % 2 == 0 {
        (pair & 0xf000) | cluster as u16
    } else {
        (pair & 0x000f) | (cluster as u16) << 4
    };
    disk[at..at + 2].copy_from_slice(&looped.to_le_bytes());
    // Fill the directory's cluster with live entries, so the walk never
    // meets an end marker.
    let base = probe.cluster_offset(cluster as u32).unwrap() as usize;
    let cluster_bytes = probe.g.cluster_bytes as usize;
    for entry in disk[base..base + cluster_bytes].chunks_mut(ENTRY) {
        entry[..11].copy_from_slice(b"FILLER  TXT");
        entry[11] = 0;
    }
    let mut fat = Fat::open(Image(disk)).unwrap();
    let docs = fat.lookup(ROOT, "Docs").unwrap();
    assert_eq!(fat.lookup(docs, "missing"), Err(Error::Corrupt));
}

#[test]
fn long_names_need_their_checksum() {
    let mut entry = [0u8; ENTRY];
    entry[..11].copy_from_slice(b"ALONGF~1TXT");
    let mut long = LongName::new();
    let mut raw = [0xffu8; ENTRY];
    raw[0] = 0x41;
    raw[11] = ATTR_LONG_NAME;
    raw[13] = checksum(&entry);
    for (i, unit) in "Hi.txt".encode_utf16().chain([0]).enumerate() {
        let at = [1, 3, 5, 7, 9, 14, 16][i];
        raw[at..at + 2].copy_from_slice(&unit.to_le_bytes());
    }
    long.add(&raw);
    assert_eq!(long.take(checksum(&entry)).as_deref(), Some("Hi.txt"));
    long.add(&raw);
    assert_eq!(
        long.take(checksum(&entry).wrapping_add(1)),
        None,
        "orphaned long name"
    );
}
