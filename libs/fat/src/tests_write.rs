//! Writing (ADR-0037): results read back by our reader and, when they are
//! installed, by the reference tools (fsck.fat, mtools); and crashes at
//! every barrier, with the device free to reorder the writes in between.

use std::collections::BTreeMap;
use std::process::Command;
use std::rc::Rc;
use std::string::{String, ToString};
use std::vec::Vec;
use std::{format, vec};

use super::*;

/// A disk image as an unchanged base plus the sectors written since:
/// cheap to copy, so thousands of crash states stay fast.
#[derive(Clone)]
struct Overlay {
    base: Rc<Vec<u8>>,
    sectors: BTreeMap<u64, [u8; 512]>,
}

impl Overlay {
    fn new(base: Vec<u8>) -> Self {
        Self {
            base: Rc::new(base),
            sectors: BTreeMap::new(),
        }
    }

    fn put(&mut self, offset: u64, data: &[u8]) {
        for (i, chunk) in data.chunks(512).enumerate() {
            self.sectors
                .insert(offset + i as u64 * 512, chunk.try_into().unwrap());
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = self.base.to_vec();
        for (&at, sector) in &self.sectors {
            out[at as usize..at as usize + 512].copy_from_slice(sector);
        }
        out
    }
}

impl Disk for Overlay {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<(), IoError> {
        if offset + out.len() as u64 > self.base.len() as u64 {
            return Err(IoError);
        }
        let mut done = 0;
        while done < out.len() {
            let at = offset + done as u64;
            let sector = at / 512 * 512;
            let within = (at - sector) as usize;
            let take = (512 - within).min(out.len() - done);
            let source = match self.sectors.get(&sector) {
                Some(bytes) => &bytes[within..within + take],
                None => &self.base[at as usize..at as usize + take],
            };
            out[done..done + take].copy_from_slice(source);
            done += take;
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), IoError> {
        assert!(
            offset % 512 == 0 && data.len() % 512 == 0,
            "whole sectors only"
        );
        if offset + data.len() as u64 > self.base.len() as u64 {
            return Err(IoError);
        }
        self.put(offset, data);
        Ok(())
    }

    fn writable(&self) -> bool {
        true
    }

    fn size(&self) -> u64 {
        self.base.len() as u64
    }
}

/// Records writes in epochs separated by barriers.
struct Recorder {
    live: Overlay,
    epochs: Vec<Vec<(u64, Vec<u8>)>>,
}

impl Disk for Recorder {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<(), IoError> {
        self.live.read_at(offset, out)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), IoError> {
        self.live.write_at(offset, data)?;
        self.epochs
            .last_mut()
            .unwrap()
            .push((offset, data.to_vec()));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), IoError> {
        if !self.epochs.last().unwrap().is_empty() {
            self.epochs.push(Vec::new());
        }
        Ok(())
    }

    fn writable(&self) -> bool {
        true
    }

    fn size(&self) -> u64 {
        self.live.size()
    }
}

fn image(name: &str) -> Vec<u8> {
    let blob = std::fs::read(format!(
        "{}/testdata/{name}.sparse",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
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

/// Image, and the byte offset of its volume (for mtools).
const IMAGES: [(&str, u64); 3] = [("fat12", 0), ("fat16", 1 << 20), ("fat32", 1 << 20)];

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8 ^ seed).collect()
}

fn path<D: Disk>(fat: &mut Fat<D>, path: &str) -> Result<NodeId, Error> {
    let mut node = ROOT;
    for part in path.split('/') {
        node = fat.lookup(node, part)?;
    }
    Ok(node)
}

fn read_all<D: Disk>(fat: &mut Fat<D>, node: NodeId) -> Vec<u8> {
    let mut out = vec![0u8; fat.size(node).unwrap() as usize];
    assert_eq!(fat.read_file(node, 0, &mut out), Ok(out.len()));
    out
}

fn write_in_pieces<D: Disk>(fat: &mut Fat<D>, node: NodeId, offset: u64, data: &[u8], step: usize) {
    let mut done = 0;
    while done < data.len() {
        let take = step.min(data.len() - done);
        assert_eq!(
            fat.write_file(node, offset + done as u64, &data[done..done + take]),
            Ok(take)
        );
        done += take;
    }
}

fn names<D: Disk>(fat: &mut Fat<D>, dir: NodeId) -> Vec<String> {
    let mut out = Vec::new();
    for index in 0.. {
        match fat.entry(dir, index) {
            Ok((name, _)) => out.push(name.to_string()),
            Err(Error::NotFound) => return out,
            Err(error) => panic!("{error:?}"),
        }
    }
    out
}

/// Creates and holds a node, as the file service does (unheld nodes may
/// be reused by the next lookup).
fn create_kept<D: Disk>(fat: &mut Fat<D>, dir: NodeId, name: &str, directory: bool) -> NodeId {
    let node = fat.create(dir, name, directory).unwrap();
    fat.retain(node).unwrap();
    node
}

fn lookup_kept<D: Disk>(fat: &mut Fat<D>, dir: NodeId, name: &str) -> NodeId {
    let node = fat.lookup(dir, name).unwrap();
    fat.retain(node).unwrap();
    node
}

/// The changes every image goes through, and what they should leave.
fn exercise<D: Disk>(fat: &mut Fat<D>) -> Vec<(&'static str, Vec<u8>)> {
    let notes = create_kept(fat, ROOT, "notes.txt", false);
    write_in_pieces(fat, notes, 0, b"hello from Oceans\n", 7);
    let mixed = create_kept(fat, ROOT, "Mixed Case Name.md", false);
    write_in_pieces(fat, mixed, 0, &pattern(3000, 1), 333);
    let thai = create_kept(fat, ROOT, "ไฟล์ใหม่.txt", false);
    write_in_pieces(fat, thai, 0, "เขียนได้แล้ว\n".as_bytes(), 4096);
    let dir = create_kept(fat, ROOT, "New Dir", true);
    let sub = create_kept(fat, dir, "sub", true);
    let deep = create_kept(fat, sub, "deep file.bin", false);
    let mut big = pattern(70_000, 2);
    write_in_pieces(fat, deep, 0, &big, 4096);
    // Overwrite in the middle, then write past the end (the gap is zeros).
    write_in_pieces(fat, deep, 10_000, &pattern(5000, 3), 97);
    big[10_000..15_000].copy_from_slice(&pattern(5000, 3));
    fat.write_file(deep, 71_000, b"tail").unwrap();
    big.resize(71_000, 0);
    big.extend_from_slice(b"tail");
    // Shrink, and empty then refill.
    fat.truncate(mixed, 100).unwrap();
    let empty_then = create_kept(fat, ROOT, "refilled.dat", false);
    write_in_pieces(fat, empty_then, 0, &pattern(9000, 4), 4096);
    fat.truncate(empty_then, 0).unwrap();
    fat.write_file(empty_then, 0, b"again").unwrap();
    // Grow by truncation.
    let grown = create_kept(fat, ROOT, "grown.bin", false);
    fat.truncate(grown, 1500).unwrap();
    // Removals.
    fat.remove(ROOT, "HELLO.TXT").unwrap();
    assert_eq!(fat.remove(ROOT, "Docs"), Err(Error::NotEmpty));
    let docs = lookup_kept(fat, ROOT, "Docs");
    let notes_dir = lookup_kept(fat, docs, "notes");
    fat.remove(notes_dir, "deep.txt").unwrap();
    fat.remove(docs, "notes").unwrap();
    fat.remove(ROOT, "Docs").unwrap();
    // Removed while held: their clusters go when released.
    fat.release(notes_dir);
    fat.release(docs);
    // Refusals.
    assert_eq!(fat.create(ROOT, "NOTES.TXT", false), Err(Error::Exists));
    assert_eq!(fat.create(ROOT, "a:b", false), Err(Error::InvalidName));
    assert_eq!(fat.write_file(dir, 0, b"x"), Err(Error::IsADirectory));
    // Many long names sharing a short basis get distinct tails.
    for n in 0..12 {
        let file = create_kept(fat, dir, &format!("Long File Name {n}.txt"), false);
        fat.write_file(file, 0, format!("{n}\n").as_bytes())
            .unwrap();
    }
    // Renames (ADR-0038): a new long name, a change of case only, a move
    // to another directory, replacing a file, moving a directory.
    let before = create_kept(fat, ROOT, "before.txt", false);
    fat.write_file(before, 0, b"renamed\n").unwrap();
    fat.rename(ROOT, "before.txt", ROOT, "After Rename.txt")
        .unwrap();
    assert_eq!(fat.lookup(ROOT, "before.txt"), Err(Error::NotFound));
    assert!(
        read_all(fat, before) == b"renamed\n",
        "the open node follows"
    );
    fat.rename(ROOT, "notes.txt", ROOT, "NOTES.txt").unwrap();
    fat.rename(ROOT, "grown.bin", dir, "grown.bin").unwrap();
    let victim = create_kept(fat, ROOT, "victim.txt", false);
    fat.write_file(victim, 0, b"victim").unwrap();
    fat.rename(ROOT, "After Rename.txt", ROOT, "victim.txt")
        .unwrap();
    assert!(
        read_all(fat, victim) == b"victim",
        "a replaced file stays readable while open"
    );
    fat.release(victim);
    fat.rename(dir, "sub", ROOT, "sub moved").unwrap();
    assert_eq!(
        fat.rename(ROOT, "New Dir", dir, "inside itself"),
        Err(Error::InvalidName)
    );
    assert_eq!(
        fat.rename(ROOT, "victim.txt", ROOT, "sub moved"),
        Err(Error::IsADirectory)
    );
    assert_eq!(fat.rename(ROOT, "missing", ROOT, "x"), Err(Error::NotFound));
    fat.sync().unwrap();
    let mut mixed_expected = pattern(3000, 1);
    mixed_expected.truncate(100);
    vec![
        ("notes.txt", b"hello from Oceans\n".to_vec()),
        ("Mixed Case Name.md", mixed_expected),
        ("ไฟล์ใหม่.txt", "เขียนได้แล้ว\n".as_bytes().to_vec()),
        ("sub moved/deep file.bin", big),
        ("victim.txt", b"renamed\n".to_vec()),
        ("refilled.dat", b"again".to_vec()),
        ("New Dir/grown.bin", vec![0u8; 1500]),
        ("New Dir/Long File Name 11.txt", b"11\n".to_vec()),
        ("A long file name.txt", b"long names work\n".to_vec()),
    ]
}

#[test]
fn writes_read_back() {
    for (name, offset) in IMAGES {
        let mut fat = Fat::open(Overlay::new(image(name))).unwrap();
        assert_eq!(fat.write_file(ROOT, 0, b"x"), Err(Error::IsADirectory));
        assert!(fat.create(ROOT, "x", false) == Err(Error::ReadOnly));
        let recovery = fat.enable_writes().unwrap();
        assert_eq!(
            recovery.reclaimed, 0,
            "{name}: the reference image has no lost clusters"
        );
        let expected = exercise(&mut fat);
        let free = fat.writes.free;
        let bytes = fat.into_disk().bytes();

        // A fresh mount of the result.
        let mut fat = Fat::open(Overlay::new(bytes.clone())).unwrap();
        for (file, content) in &expected {
            let node = path(&mut fat, file).unwrap_or_else(|e| panic!("{name}: {file}: {e:?}"));
            assert!(read_all(&mut fat, node) == *content, "{name}: {file}");
        }
        let listing = names(&mut fat, ROOT);
        assert!(
            !listing.contains(&"HELLO.TXT".to_string()) && !listing.contains(&"Docs".to_string())
        );
        assert!(
            listing.contains(&"NOTES.txt".to_string()) && listing.contains(&"New Dir".to_string())
        );
        let dir = fat.lookup(ROOT, "New Dir").unwrap();
        assert_eq!(names(&mut fat, dir).len(), 13);
        let report = fat.check().unwrap();
        assert_eq!(report.lost, 0, "{name}");
        assert_eq!(
            report.free, free,
            "{name}: the free count kept in memory is exact"
        );
        // Synced: the volume is clean, so enabling writes needs no repair.
        let recovery = fat.enable_writes().unwrap();
        assert!(name == "fat12" || !recovery.unclean, "{name}");

        reference_tools_accept(name, offset, &bytes, &expected);
    }
}

#[test]
fn files_removed_while_open_keep_their_data() {
    let mut fat = Fat::open(Overlay::new(image("fat16"))).unwrap();
    fat.enable_writes().unwrap();
    let file = fat.create(ROOT, "open.bin", false).unwrap();
    fat.write_file(file, 0, &pattern(5000, 9)).unwrap();
    fat.retain(file).unwrap();
    let before = fat.check().unwrap().free;
    fat.remove(ROOT, "open.bin").unwrap();
    assert_eq!(fat.lookup(ROOT, "open.bin"), Err(Error::NotFound));
    assert!(
        read_all(&mut fat, file) == pattern(5000, 9),
        "still readable"
    );
    // A new file may reuse the entry slot without touching the open one.
    let other = fat.create(ROOT, "other.bin", false).unwrap();
    fat.write_file(other, 0, b"other").unwrap();
    assert!(read_all(&mut fat, file) == pattern(5000, 9));
    fat.release(file);
    let after = fat.check().unwrap();
    assert_eq!(after.lost, 0);
    assert!(after.free > before - 2, "the clusters came back");
}

#[test]
fn fills_up_cleanly() {
    let mut fat = Fat::open(Overlay::new(image("fat12"))).unwrap();
    fat.enable_writes().unwrap();
    let file = fat.create(ROOT, "huge.bin", false).unwrap();
    let chunk = vec![7u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        match fat.write_file(file, written, &chunk) {
            Ok(n) => written += n as u64,
            Err(Error::NoSpace) => break,
            Err(error) => panic!("{error:?}"),
        }
    }
    assert!(written > 1_000_000, "a 1.44 MB floppy holds most of a MiB");
    let report = fat.check().unwrap();
    assert_eq!(report.lost, 0, "a refused write leaves nothing allocated");
    fat.truncate(file, 0).unwrap();
    assert!(fat.check().unwrap().free > 2700);
}

/// A crash at every barrier, with random subsets of the writes in flight.
#[test]
fn every_crash_leaves_a_repairable_volume() {
    for (name, offset) in IMAGES {
        let base = image(name);
        let mut fat = Fat::open(Recorder {
            live: Overlay::new(base.clone()),
            epochs: vec![Vec::new()],
        })
        .unwrap();
        fat.enable_writes().unwrap();
        let expected = exercise(&mut fat);
        let epochs = fat.into_disk().epochs;
        assert!(epochs.len() > 50, "{name}: {} barriers", epochs.len());
        let base = Rc::new(base);
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut states = 0;
        let mut prefix = Overlay {
            base: base.clone(),
            sectors: BTreeMap::new(),
        };
        for epoch in &epochs {
            for variant in 0..3 {
                let mut crashed = prefix.clone();
                for (at, data) in epoch {
                    // Variant 0: nothing of this epoch arrived; the others:
                    // a random part of it.
                    if variant > 0 && random() % 2 == 0 {
                        crashed.put(*at, data);
                    }
                }
                states += 1;
                let mut fat = Fat::open(crashed)
                    .unwrap_or_else(|e| panic!("{name}: mount after a crash: {e:?}"));
                fat.enable_writes()
                    .unwrap_or_else(|e| panic!("{name}: repair after a crash: {e:?}"));
                let report = fat
                    .check()
                    .unwrap_or_else(|e| panic!("{name}: check after repair: {e:?}"));
                assert_eq!(
                    (
                        report.lost,
                        report.orphans,
                        report.overlong,
                        report.duplicates,
                        report.parents
                    ),
                    (0, 0, 0, 0, 0),
                    "{name}"
                );
                read_everything(&mut fat, ROOT);
                if states % 97 == 0 {
                    fat.sync().unwrap();
                    let bytes = fat.into_disk().bytes();
                    reference_tools_accept(name, offset, &bytes, &[]);
                }
            }
            for (at, data) in epoch {
                prefix.put(*at, data);
            }
        }
        // All of it arrived: exactly the expected result.
        let mut fat = Fat::open(prefix).unwrap();
        for (file, content) in &expected {
            let node = path(&mut fat, file).unwrap();
            assert!(read_all(&mut fat, node) == *content, "{name}: {file}");
        }
        assert!(states > 150);
    }
}

/// Reads every file of a tree in full (no error may come out).
fn read_everything<D: Disk>(fat: &mut Fat<D>, dir: NodeId) {
    for name in names(fat, dir) {
        let node = lookup_kept(fat, dir, &name);
        if fat.is_directory(node).unwrap() {
            read_everything(fat, node);
        } else {
            read_all(fat, node);
        }
        fat.release(node);
    }
}

// ---- The reference tools ---------------------------------------------------

/// Runs a dosfstools/mtools command, when available: on the PATH (Linux
/// CI installs them), or in WSL below `OCEANS_FAT_TOOLS_WSL` (the unpacked
/// packages; see testdata/generate.sh). `None`: not available.
fn tool(args: &[&str]) -> Option<(bool, Vec<u8>, Vec<u8>)> {
    let output = match std::env::var("OCEANS_FAT_TOOLS_WSL") {
        Ok(root) => {
            let quoted: Vec<String> = args.iter().map(|a| format!("'{}'", wsl_path(a))).collect();
            let script = format!(
                "export LANG=C.UTF-8 MTOOLS_SKIP_CHECK=1 PATH={root}/usr/sbin:{root}/usr/bin:$PATH; {}",
                quoted.join(" ")
            );
            Command::new("wsl")
                .args(["-e", "sh", "-c", &script])
                .output()
                .ok()?
        }
        Err(_) => Command::new(args[0])
            .args(&args[1..])
            .env("MTOOLS_SKIP_CHECK", "1")
            .env("LANG", "C.UTF-8")
            .output()
            .ok()?,
    };
    // 127: the shell found no such command.
    if output.status.code() == Some(127) {
        return None;
    }
    Some((output.status.success(), output.stdout, output.stderr))
}

/// `C:\x\y` as WSL sees it (`/mnt/c/x/y`); anything else unchanged.
fn wsl_path(arg: &str) -> String {
    let bytes = arg.as_bytes();
    if bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        let rest = arg[3..].replace('\\', "/");
        let (rest, suffix) = match rest.split_once("@@") {
            Some((file, offset)) => (file.to_string(), format!("@@{offset}")),
            None => (rest, String::new()),
        };
        format!(
            "/mnt/{}/{rest}{suffix}",
            (bytes[0] as char).to_ascii_lowercase()
        )
    } else {
        arg.to_string()
    }
}

/// fsck.fat finds nothing to fix, and mtools reads our files the same.
fn reference_tools_accept(name: &str, offset: u64, bytes: &[u8], expected: &[(&str, Vec<u8>)]) {
    // Tests run in parallel: every image gets its own file.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file = std::env::temp_dir().join(format!(
        "oceans-fat-{name}-{}-{serial}.img",
        std::process::id()
    ));
    let file_name = file.to_str().unwrap().to_string();
    // fsck.fat works on a volume: cut the partition out.
    let volume = &bytes[offset as usize..];
    std::fs::write(&file, volume).unwrap();
    let Some((clean, output, _)) = tool(&["fsck.fat", "-n", "-v", &file_name]) else {
        std::eprintln!("fsck.fat not available: reference check skipped");
        return;
    };
    let text = String::from_utf8_lossy(&output);
    assert!(clean, "{name}: fsck.fat found problems:\n{text}");
    for (path, content) in expected {
        let target = format!("::/{path}");
        let (ok, data, errors) =
            tool(&["mtype", "-i", &file_name, &target]).expect("mtools with dosfstools");
        assert!(
            ok,
            "{name}: mtype {path}: {}",
            String::from_utf8_lossy(&errors)
        );
        assert!(
            data == *content,
            "{name}: mtools reads {path} differently: {:?}",
            String::from_utf8_lossy(&data)
        );
    }
    if std::env::var("OCEANS_FAT_KEEP").is_err() {
        let _ = std::fs::remove_file(&file);
    }
}
