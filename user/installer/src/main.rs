//! installer: installs Oceans over the disk it was started from (ADR-0127).
//!
//! Started by init in install mode (`oceans.mode=install`), which the
//! Windows program (ADR-0128) arranges after the user has confirmed, there,
//! that Windows and everything on the disk go. It runs once, then
//! restarts the machine into the installed system:
//!
//! 1. **The disk:** the NVMe SSD whose GPT holds the partition the kernel
//!    was loaded from (the staging partition the Windows program made). No
//!    other disk is ever written; started from anywhere else, it refuses.
//! 2. **What to install:** from the staging partition, `oceans/system.opk`,
//!    the signed release. It is checked against the keys Oceans itself
//!    trusts (`update.keys`, the same as `update`). Everything written to
//!    the disk comes from it: the kernel, the boot archive, Limine
//!    (`limine.efi`). `oceans/install` names the ESP's GUID, which the UEFI
//!    boot entry the Windows program made points to.
//! 3. **The layout** (`oceans-install`): a new GPT with that ESP and an
//!    Oceans root, the release in slot `a`, the root blank (formatted at
//!    first boot, ADR-0126).
//!
//! What it reads is held in memory before anything is written: the
//! staging partition is overwritten by the new layout.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_block_proto::{Disk, SECTOR_SIZE};
use oceans_fat::{Fat, ROOT, Window};
use oceans_install::{Guids, Payload};
use oceans_package::{Package, TrustedKey};
use oceans_rt::{Buffer, Directory, Handle, Start, prot};

oceans_rt::entry!(main);

/// Bytes per block request.
const CHUNK: usize = 128 * 1024;
/// The largest release read.
const MAX_PACKAGE: u64 = 256 << 20;
const SYSTEM_ID: &str = "system.oceans";

const EXIT_BAD_START: i64 = 2;
const EXIT_REFUSED: i64 = 3;

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<200>::new();
    let _ = line.write_str("installer: ");
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return EXIT_BAD_START;
    };
    let Some(log) = directory.find("log", "log") else {
        return EXIT_BAD_START;
    };
    match install(log, &directory) {
        Ok(release) => {
            say(
                log,
                format_args!("Oceans {release} is installed; restarting into it"),
            );
            if let Some(power) = directory.find("power", "power") {
                let _ = oceans_rt::request_power(power, oceans_rt::power::RESTART);
            }
            0
        }
        Err(problem) => {
            say(log, format_args!("not installed: {problem}"));
            say(
                log,
                format_args!(
                    "nothing was written to the disk unless said above; restart to go back"
                ),
            );
            EXIT_REFUSED
        }
    }
}

fn install(log: Handle, directory: &Directory) -> Result<String, String> {
    let sysinfo = directory
        .find("sysinfo", "sysinfo")
        .ok_or("no system information")?;
    let block = directory.find("use", "nvme").ok_or("no NVMe SSD")?;
    let keys_module = directory
        .find("module", "update.keys")
        .ok_or("no update keys in the image")?;
    let staging = oceans_rt::boot_partition(sysinfo)
        .ok_or("Oceans was not started from a GPT partition (the Windows program prepares one)")?;

    let mut disk = BlockDisk::open(block)?;
    let sectors = disk.sectors;
    // 1. The disk: the one holding the partition Oceans started from.
    let partitions = oceans_gpt::read(&mut disk)
        .map_err(|_| "the SSD has no partition table: Oceans was not started from it")?;
    let staged = partitions
        .into_iter()
        .flatten()
        .find(|p| p.guid == staging)
        .ok_or("Oceans was not started from the NVMe SSD; nothing to install over")?;
    say(
        log,
        format_args!(
            "started from the SSD's partition \"{}\" ({} MiB of {} MiB)",
            staged.name,
            staged.size() >> 20,
            (sectors * 512) >> 20
        ),
    );

    // 2. What to install, read whole before anything is written.
    let window = Window::new(&mut disk, staged.start(), staged.size())
        .ok_or("the staging partition is outside the disk")?;
    let mut fat = Fat::open(window).map_err(|_| "the staging partition is not FAT")?;
    let package_bytes = read_file(&mut fat, &["oceans", "system.opk"], MAX_PACKAGE)?;
    let order = read_file(&mut fat, &["oceans", "install"], 4096)?;
    drop(fat);
    let order = core::str::from_utf8(&order).map_err(|_| "oceans/install is not text")?;
    let esp_guid = order
        .lines()
        .find_map(|line| line.strip_prefix("esp = "))
        .and_then(|text| oceans_install::parse_guid(text.trim()))
        .ok_or("oceans/install names no ESP GUID")?;

    let keys_text = module_text(keys_module)?;
    let keys: Vec<TrustedKey<'_>> = oceans_package::trusted_keys(&keys_text).collect();
    if keys.is_empty() {
        return Err("this image trusts no release keys".into());
    }
    let package = Package::open(&package_bytes, &keys)
        .map_err(|e| alloc::format!("the release is refused: {}", e.message()))?;
    let manifest = &package.manifest;
    if manifest.id != SYSTEM_ID {
        return Err(alloc::format!("{} is not an Oceans release", manifest.id));
    }
    let (Some(kernel), Some(initrd), Some(limine)) = (
        package.file(oceans_install::KERNEL),
        package.file(oceans_install::INITRD),
        package.file("limine.efi"),
    ) else {
        return Err("the release lacks the kernel, the boot archive or Limine".into());
    };
    let release = alloc::format!("{} {}", manifest.version, manifest.channel);
    say(
        log,
        format_args!(
            "Oceans {release} from {}, verified; installing over the whole SSD",
            manifest.publisher
        ),
    );

    // 3. The layout.
    let guids = Guids {
        disk: random_guid()?,
        esp: esp_guid,
        root: random_guid()?,
    };
    let plan = oceans_install::plan(sectors, guids).map_err(|e| e.message())?;
    say(
        log,
        format_args!(
            "writing: a new partition table, the EFI system partition ({} MiB), the Oceans root ({} MiB)",
            plan.esp.size() >> 20,
            plan.root.size() >> 20
        ),
    );
    let payload = Payload {
        limine,
        kernel,
        initrd,
        release: &release,
    };
    let mut disk =
        oceans_install::lay_out(disk, sectors, guids, &plan, &payload).map_err(|e| e.message())?;
    disk.flush()?;
    Ok(release)
}

/// A whole file of the FAT volume, at most `limit` bytes.
fn read_file<D: oceans_fat::Disk>(
    fat: &mut Fat<D>,
    path: &[&str],
    limit: u64,
) -> Result<Vec<u8>, String> {
    let mut node = ROOT;
    for name in path {
        node = fat
            .lookup(node, name)
            .map_err(|_| alloc::format!("the staging partition has no {}", path.join("/")))?;
    }
    let size = fat.size(node).map_err(|_| "a staged file cannot be read")?;
    if size > limit {
        return Err(alloc::format!("{} is too large", path.join("/")));
    }
    let mut bytes = vec![0u8; size as usize];
    let mut done = 0;
    while done < bytes.len() {
        let got = fat
            .read_file(node, done as u64, &mut bytes[done..])
            .map_err(|_| "a staged file cannot be read")?;
        if got == 0 {
            return Err("a staged file is shorter than it says".into());
        }
        done += got;
    }
    Ok(bytes)
}

/// A module's text, without the zeros that pad it to a page.
fn module_text(module: Handle) -> Result<String, String> {
    let size = oceans_rt::memory_size(module).map_err(|_| "the update keys cannot be read")?;
    let base = oceans_rt::memory_map(module, 0, prot::READ)
        .map_err(|_| "the update keys cannot be read")?;
    // SAFETY: `base` maps `size` bytes of the module, read-only, until it is
    // unmapped below; they are copied out first.
    let bytes = unsafe { core::slice::from_raw_parts(base, size as usize) };
    let end = bytes
        .iter()
        .rposition(|&b| b != 0)
        .map_or(0, |last| last + 1);
    let text = core::str::from_utf8(&bytes[..end]).map(String::from);
    let _ = oceans_rt::memory_unmap(base);
    text.map_err(|_| "the update keys are not text".into())
}

/// A random (version 4) GUID, as stored on disk.
fn random_guid() -> Result<[u8; 16], String> {
    let mut guid = [0u8; 16];
    oceans_rt::random(&mut guid).map_err(|_| "no randomness")?;
    guid[7] = (guid[7] & 0x0f) | 0x40;
    guid[8] = (guid[8] & 0x3f) | 0x80;
    Ok(guid)
}

/// The whole SSD, through a block session, as the FAT and GPT code wants
/// it: byte offsets in whole sectors.
struct BlockDisk {
    disk: Disk,
    sectors: u64,
}

impl BlockDisk {
    fn open(block: Handle) -> Result<Self, String> {
        let disk = Disk::open(block, CHUNK).map_err(|_| "the SSD cannot be opened")?;
        if disk.info.read_only() || disk.info.sector_size as usize != SECTOR_SIZE {
            return Err("the SSD is read-only or has unusual sectors".into());
        }
        let sectors = disk.info.sectors;
        Ok(Self { disk, sectors })
    }

    fn flush(&mut self) -> Result<(), String> {
        self.disk
            .flush()
            .map_err(|_| "the SSD could not be flushed".into())
    }
}

impl oceans_fat::Disk for BlockDisk {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<(), oceans_fat::IoError> {
        let sector = SECTOR_SIZE as u64;
        let mut done = 0;
        while done < out.len() {
            let at = offset + done as u64;
            let first = at / sector;
            let skip = (at % sector) as usize;
            let want = (out.len() - done + skip)
                .div_ceil(SECTOR_SIZE)
                .min(CHUNK / SECTOR_SIZE);
            let count = want.min((self.sectors.saturating_sub(first)) as usize);
            if count == 0 {
                return Err(oceans_fat::IoError);
            }
            self.disk
                .read(first, count as u32, 0)
                .map_err(|_| oceans_fat::IoError)?;
            let take = (count * SECTOR_SIZE - skip).min(out.len() - done);
            out[done..done + take].copy_from_slice(&self.disk.buffer()[skip..skip + take]);
            done += take;
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), oceans_fat::IoError> {
        let sector = SECTOR_SIZE as u64;
        if !offset.is_multiple_of(sector) || !(data.len() as u64).is_multiple_of(sector) {
            return Err(oceans_fat::IoError);
        }
        for (i, chunk) in data.chunks(CHUNK).enumerate() {
            let first = offset / sector + (i * CHUNK / SECTOR_SIZE) as u64;
            if first + (chunk.len() / SECTOR_SIZE) as u64 > self.sectors {
                return Err(oceans_fat::IoError);
            }
            self.disk.buffer()[..chunk.len()].copy_from_slice(chunk);
            self.disk
                .write(first, (chunk.len() / SECTOR_SIZE) as u32, 0)
                .map_err(|_| oceans_fat::IoError)?;
        }
        Ok(())
    }

    fn writable(&self) -> bool {
        true
    }

    fn size(&self) -> u64 {
        self.sectors * SECTOR_SIZE as u64
    }
}
