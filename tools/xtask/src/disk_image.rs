//! Disk images made here, in Rust (ADR-0092): FAT32 volumes from
//! `oceans-fat`, GPT from `oceans-gpt`. No dosfstools or mtools needed;
//! where they are installed (CI), `fsck.fat` still checks the results.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use oceans_fat::{Disk, Fat, IoError, NodeId, ROOT, Window};
use oceans_gpt::Partition;

use super::*;

const SECTOR: u64 = 512;
/// Partitions start on 1 MiB boundaries.
const ALIGN: u64 = 2048;

/// A disk image file.
pub struct FileDisk {
    file: File,
    size: u64,
}

impl FileDisk {
    /// A new image of `size` bytes (sparse where the host allows), all
    /// zero.
    pub fn create(path: &Path, size: u64) -> Result<Self> {
        if path.exists() {
            fs::remove_file(path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
        }
        let file = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        file.set_len(size)
            .map_err(|e| format!("cannot size {}: {e}", path.display()))?;
        Ok(Self { file, size })
    }

    pub fn open(path: &Path) -> Result<Self> {
        let file = File::options()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let size = file.metadata().map_err(|e| e.to_string())?.len();
        Ok(Self { file, size })
    }
}

impl Disk for FileDisk {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> std::result::Result<(), IoError> {
        if offset + out.len() as u64 > self.size {
            return Err(IoError);
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(out))
            .map_err(|_| IoError)
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> std::result::Result<(), IoError> {
        if offset + data.len() as u64 > self.size {
            return Err(IoError);
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.write_all(data))
            .map_err(|_| IoError)
    }

    fn writable(&self) -> bool {
        true
    }

    fn size(&self) -> u64 {
        self.size
    }
}

/// Bytes of the files below `dir`.
pub fn tree_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        total += if path.is_dir() {
            tree_size(&path)?
        } else {
            entry.metadata().map_err(|e| e.to_string())?.len()
        };
    }
    Ok(total)
}

/// A volume's serial number, stable per label.
fn serial(label: &str) -> u32 {
    oceans_gpt::crc32(label.as_bytes())
}

/// Formats `disk` (a whole volume) as FAT32 and copies `sources` into its
/// root: folders whole (their contents, under their own name) and files.
pub fn fat_volume<D: Disk>(disk: D, label: &str, sources: &[&Path]) -> Result<D> {
    let mut disk = disk;
    oceans_fat::format(&mut disk, label, serial(label))
        .map_err(|e| format!("cannot format the {label} volume: {e:?}"))?;
    let mut fat = Fat::open(disk).map_err(|e| format!("the new {label} volume: {e:?}"))?;
    fat.enable_writes()
        .map_err(|e| format!("the new {label} volume: {e:?}"))?;
    for source in sources {
        copy_in(&mut fat, ROOT, source)?;
    }
    fat.sync()
        .map_err(|e| format!("the {label} volume: {e:?}"))?;
    Ok(fat.into_disk())
}

/// Copies `source` (a file or a folder) into directory `dir`.
fn copy_in<D: Disk>(fat: &mut Fat<D>, dir: NodeId, source: &Path) -> Result {
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{}: no name", source.display()))?;
    if source.is_dir() {
        let node = fat
            .create(dir, name, true)
            .map_err(|e| format!("{}: {e:?}", source.display()))?;
        // Held while its children are made: a node nothing holds can be
        // reused by the next lookup.
        fat.retain(node).map_err(|e| format!("{name}: {e:?}"))?;
        let mut children: Vec<PathBuf> = fs::read_dir(source)
            .map_err(|e| format!("{}: {e}", source.display()))?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<std::io::Result<_>>()
            .map_err(|e| e.to_string())?;
        children.sort();
        for child in children {
            copy_in(fat, node, &child)?;
        }
        fat.release(node);
        Ok(())
    } else {
        let bytes = fs::read(source).map_err(|e| format!("{}: {e}", source.display()))?;
        put_file(fat, dir, name, &bytes)
    }
}

/// Writes a whole file `name` into directory `dir`, replacing one there.
pub fn put_file<D: Disk>(fat: &mut Fat<D>, dir: NodeId, name: &str, bytes: &[u8]) -> Result {
    let _ = fat.remove(dir, name);
    let node = fat
        .create(dir, name, false)
        .map_err(|e| format!("{name}: {e:?}"))?;
    fat.retain(node).map_err(|e| format!("{name}: {e:?}"))?;
    let mut done = 0;
    while done < bytes.len() {
        let wrote = fat
            .write_file(node, done as u64, &bytes[done..])
            .map_err(|e| format!("{name}: {e:?}"))?;
        if wrote == 0 {
            return Err(format!("{name}: the volume is full"));
        }
        done += wrote;
    }
    fat.release(node);
    Ok(())
}

/// One partition of a GPT image to make.
pub struct Volume<'a> {
    pub kind: [u8; 16],
    /// The GPT entry's name.
    pub name: &'a str,
    /// The FAT volume's label.
    pub label: &'a str,
    pub mib: u64,
    /// What goes in its root.
    pub sources: Vec<PathBuf>,
}

/// A GPT disk image at `path` holding `volumes` in entries 1, 2, ...,
/// each formatted FAT32 with its files. `seed` makes the GUIDs (stable
/// per build).
pub fn gpt_image(path: &Path, volumes: &[Volume<'_>], seed: &[u8]) -> Result {
    use sha2::Digest;
    let guid = |what: &[u8]| {
        let digest = sha2::Sha256::new()
            .chain_update(seed)
            .chain_update(what)
            .finalize();
        let mut guid = [0u8; 16];
        guid.copy_from_slice(&digest[..16]);
        // Random-variant GUIDs (version 4, RFC 4122 variant).
        guid[7] = (guid[7] & 0x0f) | 0x40;
        guid[8] = (guid[8] & 0x3f) | 0x80;
        guid
    };
    let mut partitions = Vec::new();
    let mut next = ALIGN;
    for volume in volumes {
        let sectors = volume.mib * 2048;
        partitions.push(Partition {
            kind: volume.kind,
            guid: guid(volume.label.as_bytes()),
            first_lba: next,
            last_lba: next + sectors - 1,
            name: volume.name.to_string(),
        });
        next += sectors;
    }
    let total = next + ALIGN;
    let mut disk = FileDisk::create(path, total * SECTOR)?;
    for (lba, bytes) in oceans_gpt::create(total, guid(b"disk"), &partitions) {
        disk.write_at(lba * SECTOR, &bytes)
            .map_err(|_| format!("cannot write {}", path.display()))?;
    }
    for (volume, partition) in volumes.iter().zip(&partitions) {
        let window = Window::new(&mut disk, partition.start(), partition.size())
            .ok_or("a partition outside the image")?;
        let sources: Vec<&Path> = volume.sources.iter().map(PathBuf::as_path).collect();
        fat_volume(window, volume.label, &sources)?;
    }
    Ok(())
}

/// Opens partition `entry` (from 1) of the GPT image at `path` as FAT,
/// writable.
pub fn open_partition(path: &Path, entry: usize) -> Result<Fat<Window<FileDisk>>> {
    let mut disk = FileDisk::open(path)?;
    let partition = oceans_gpt::read(&mut disk)
        .map_err(|e| format!("{}: {e:?}", path.display()))?
        .get(entry - 1)
        .cloned()
        .flatten()
        .ok_or_else(|| format!("{}: no partition {entry}", path.display()))?;
    let window = Window::new(disk, partition.start(), partition.size())
        .ok_or("a partition outside the image")?;
    let mut fat = Fat::open(window).map_err(|e| format!("partition {entry}: {e:?}"))?;
    fat.enable_writes()
        .map_err(|e| format!("partition {entry}: {e:?}"))?;
    Ok(fat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_gpt_image_with_volumes_we_can_read() {
        let dir = std::env::temp_dir().join(format!("oceans-disk-image-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let content = dir.join("content");
        fs::create_dir_all(content.join("EFI").join("BOOT")).unwrap();
        fs::write(content.join("EFI").join("BOOT").join("BOOTX64.EFI"), b"efi").unwrap();
        fs::write(content.join("limine.conf"), b"timeout: 3\n").unwrap();
        let image = dir.join("disk.img");
        gpt_image(
            &image,
            &[
                Volume {
                    kind: oceans_gpt::EFI_SYSTEM,
                    name: "EFI system partition",
                    label: "OCEANS",
                    mib: 40,
                    sources: vec![content.join("EFI"), content.join("limine.conf")],
                },
                Volume {
                    kind: oceans_gpt::OCEANS_SLOT,
                    name: "Oceans slot",
                    label: "OCEANS-B",
                    mib: 40,
                    sources: Vec::new(),
                },
            ],
            b"seed",
        )
        .unwrap();
        let mut fat = open_partition(&image, 1).unwrap();
        assert_eq!(fat.label(), "OCEANS");
        let efi = fat.lookup(ROOT, "EFI").unwrap();
        let boot = fat.lookup(efi, "BOOT").unwrap();
        let file = fat.lookup(boot, "BOOTX64.EFI").unwrap();
        let mut back = [0u8; 3];
        fat.read_file(file, 0, &mut back).unwrap();
        assert_eq!(&back, b"efi");
        put_file(&mut fat, ROOT, "limine.conf", b"timeout: 0\n").unwrap();
        fat.sync().unwrap();
        let report = fat.check().unwrap();
        assert_eq!(report.lost + report.orphans, 0);
        let slot = open_partition(&image, 2).unwrap();
        assert_eq!(slot.label(), "OCEANS-B");
        // Our reader finds the first FAT partition on the whole disk.
        let whole = Fat::open(FileDisk::open(&image).unwrap()).unwrap();
        assert_eq!(whole.label(), "OCEANS");
        let _ = fs::remove_dir_all(&dir);
    }
}
