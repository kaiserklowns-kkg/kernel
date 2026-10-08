//! Updates on a stick that boots with Secure Boot (ADR-0092).
//!
//! ```text
//! entry 1  EFI system partition   the selector: a signed Limine whose
//!                                 configuration never changes; /usb
//! entry 2  Oceans slot            the release that starts first
//! entry 3  Oceans slot            the previous release (or empty)
//! ```
//!
//! The selector's "Oceans" starts whatever partition is GPT entry 2 and
//! "previous" entry 3, through each slot's own signed Limine. An update
//! is written whole into the partition at entry 3, then the two entries
//! are swapped: one sector, written so that a power cut at any moment
//! leaves one order every reader agrees on (`oceans-gpt`).
//!
//! The update carries the slot's signed Limine and its configuration
//! (made with the Secure Boot key when the release is built); the device
//! cannot sign. Before anything is written, the configuration must name
//! this kernel and boot archive by hash, and Limine must carry the
//! configuration's hash: an update that would not boot is refused.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use blake2::{Blake2b512, Digest};
use oceans_block_proto::{Disk, SECTOR_SIZE};
use oceans_fat::{Fat, IoError, NodeId, ROOT, Window};
use oceans_gpt::Partition;
use oceans_rt::{Handle, Out};

/// The slot's files (as `cargo xtask` writes them, `secure_boot.rs`).
const EFI_DIR: [&str; 2] = ["EFI", "BOOT"];
const EFI_NAME: &str = "BOOTX64.EFI";
const CONFIG: &str = "limine.conf";
const KERNEL: &str = "oceans-kernel";
const INITRD: &str = "initrd";
const RELEASE: &str = "release";
/// In the update package.
pub const UPDATE_EFI: &str = "BOOTX64.EFI";
pub const UPDATE_CONFIG: &str = "limine.conf";
/// Where Limine keeps its configuration's hash.
const CONFIG_MARKER: &[u8] = b"++CONFIG_B2SUM_SIGNATURE++";
/// Bytes moved per disk request.
const BUFFER: usize = 64 * 1024;
/// The slot that starts first, and the previous one.
const FIRST: usize = 2;
const SECOND: usize = 3;

/// The stick, through the block service: byte reads, sector writes.
pub struct Stick {
    disk: Disk,
    sectors: u64,
    writable: bool,
}

impl Stick {
    pub fn open(block: Handle) -> Result<Self, String> {
        let info =
            oceans_block_proto::info(block).map_err(|e| format!("the stick: {}", e.message()))?;
        let disk = Disk::open(block, BUFFER).map_err(|e| format!("the stick: {}", e.message()))?;
        Ok(Self {
            disk,
            sectors: info.sectors,
            writable: !info.read_only(),
        })
    }
}

impl oceans_fat::Disk for Stick {
    fn read_at(&mut self, mut offset: u64, out: &mut [u8]) -> Result<(), IoError> {
        let per_read = (BUFFER / SECTOR_SIZE) as u64;
        let mut done = 0;
        while done < out.len() {
            let sector = offset / SECTOR_SIZE as u64;
            if sector >= self.sectors {
                return Err(IoError);
            }
            let skip = (offset % SECTOR_SIZE as u64) as usize;
            let wanted = (skip + out.len() - done).div_ceil(SECTOR_SIZE) as u64;
            let count = per_read.min(self.sectors - sector).min(wanted);
            self.disk
                .read(sector, count as u32, 0)
                .map_err(|_| IoError)?;
            let take = (count as usize * SECTOR_SIZE - skip).min(out.len() - done);
            out[done..done + take].copy_from_slice(&self.disk.buffer()[skip..skip + take]);
            done += take;
            offset += take as u64;
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), IoError> {
        let size = SECTOR_SIZE as u64;
        if !offset.is_multiple_of(size) || !(data.len() as u64).is_multiple_of(size) {
            return Err(IoError);
        }
        let first = offset / size;
        if first + data.len() as u64 / size > self.sectors {
            return Err(IoError);
        }
        for (i, chunk) in data.chunks(BUFFER).enumerate() {
            let sector = first + (i * BUFFER / SECTOR_SIZE) as u64;
            self.disk.buffer()[..chunk.len()].copy_from_slice(chunk);
            self.disk
                .write(sector, (chunk.len() / SECTOR_SIZE) as u32, 0)
                .map_err(|_| IoError)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), IoError> {
        self.disk.flush().map_err(|_| IoError)
    }

    fn writable(&self) -> bool {
        self.writable
    }

    fn size(&self) -> u64 {
        self.sectors * SECTOR_SIZE as u64
    }
}

fn gpt_error(error: oceans_gpt::Error) -> String {
    match error {
        oceans_gpt::Error::Io => "the stick cannot be read".into(),
        oceans_gpt::Error::NotGpt => "the stick has no partition table".into(),
        _ => "the stick's partition table is not one Oceans makes".into(),
    }
}

fn fat_error(what: &str) -> impl Fn(oceans_fat::Error) -> String + '_ {
    move |error| format!("{what}: {error:?}")
}

/// A slot partition: where it is, its name, and the release it holds.
struct Slot {
    partition: Partition,
    /// `A` or `B`, from the volume label (`OCEANS-A`).
    name: String,
    release: Option<String>,
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn blake2b(bytes: &[u8]) -> String {
    hex(&Blake2b512::digest(bytes))
}

/// Partition `partition` of the stick as a FAT volume.
fn volume<'a>(
    stick: &'a mut Stick,
    partition: &Partition,
) -> Result<Fat<Window<&'a mut Stick>>, String> {
    let window = Window::new(stick, partition.start(), partition.size())
        .ok_or("a slot lies outside the stick")?;
    Fat::open(window).map_err(fat_error("a slot's volume"))
}

/// Reads file `name` in `dir` of `fat`, if it is there.
fn read_file<D: oceans_fat::Disk>(fat: &mut Fat<D>, dir: NodeId, name: &str) -> Option<Vec<u8>> {
    let file = fat.lookup(dir, name).ok()?;
    let mut bytes = alloc::vec![0u8; usize::try_from(fat.size(file).ok()?).ok()?];
    let read = fat.read_file(file, 0, &mut bytes).ok()?;
    bytes.truncate(read);
    Some(bytes)
}

/// The slots at entries 2 and 3, after making the table whole.
fn slots(stick: &mut Stick) -> Result<[Slot; 2], String> {
    let table = oceans_gpt::read(stick).map_err(gpt_error)?;
    let entry = |n: usize| table.get(n - 1).cloned().flatten();
    let is_slot = |p: &Option<Partition>| {
        p.as_ref()
            .is_some_and(|p| p.kind == oceans_gpt::OCEANS_SLOT)
    };
    let first_is_esp = entry(1).is_some_and(|p| p.kind == oceans_gpt::EFI_SYSTEM);
    if !first_is_esp || !is_slot(&entry(FIRST)) || !is_slot(&entry(SECOND)) {
        return Err("the stick does not have Oceans' Secure Boot partitions (ADR-0092)".into());
    }
    let read = |stick: &mut Stick, number: usize| -> Slot {
        let partition = entry(number).expect("checked above");
        let (name, release) = match volume(stick, &partition) {
            Ok(mut fat) => {
                let label = fat.label().to_string();
                let name = label
                    .strip_prefix("OCEANS-")
                    .map_or_else(|| format!("{number}"), ToString::to_string);
                let release = read_file(&mut fat, ROOT, RELEASE)
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .map(|text| text.trim().to_string());
                (name, release)
            }
            Err(_) => (format!("{number}"), None),
        };
        Slot {
            partition,
            name,
            release,
        }
    };
    Ok([read(stick, FIRST), read(stick, SECOND)])
}

/// Makes the table whole after an interrupted switch, saying so.
fn repair(out: &mut Out, stick: &mut Stick) -> Result<(), String> {
    if !stick.writable {
        return Ok(());
    }
    match oceans_gpt::repair(stick).map_err(gpt_error)? {
        oceans_gpt::Repair::Nothing => {}
        _ => {
            let _ = writeln!(
                out,
                "update: finished an interrupted switch of the boot slots"
            );
        }
    }
    Ok(())
}

pub fn status(out: &mut Out, block: Handle, release: &str) -> Result<(), String> {
    let mut stick = Stick::open(block)?;
    repair(out, &mut stick)?;
    let [first, second] = slots(&mut stick)?;
    let running = [&first, &second]
        .into_iter()
        .find(|slot| slot.release.as_deref() == Some(release))
        .ok_or_else(|| format!("the running release ({release}) is in neither slot"))?;
    let _ = writeln!(
        out,
        "update: running Oceans {release} (Secure Boot, slot {})",
        running.name
    );
    let shown = |slot: &Slot| slot.release.clone().unwrap_or_else(|| "?".into());
    let _ = writeln!(
        out,
        "update: starts first: slot {} ({})",
        first.name,
        shown(&first)
    );
    match &second.release {
        Some(previous) => {
            let _ = writeln!(out, "update: previous: slot {} ({previous})", second.name);
        }
        None => {
            let _ = writeln!(out, "update: previous: slot {} is empty", second.name);
        }
    }
    Ok(())
}

/// Checks that `config` names `kernel` and `initrd` by hash and that
/// `efi` (Limine) carries `config`'s hash: what Limine will check at boot.
fn check_boot_files(efi: &[u8], config: &[u8], kernel: &[u8], initrd: &[u8]) -> Result<(), String> {
    let text = core::str::from_utf8(config).map_err(|_| "refused: its limine.conf is not text")?;
    let names = |what: &str, bytes: &[u8]| {
        let wanted = format!("boot():/boot/{what}#{}", blake2b(bytes));
        text.lines().any(|line| {
            let line = line.trim();
            (line.strip_prefix("path: ") == Some(wanted.as_str()))
                || (line.strip_prefix("module_path: ") == Some(wanted.as_str()))
        })
    };
    if !names(KERNEL, kernel) || !names(INITRD, initrd) {
        return Err("refused: its limine.conf does not name its kernel and boot archive".into());
    }
    let at = efi
        .windows(CONFIG_MARKER.len())
        .position(|window| window == CONFIG_MARKER)
        .map(|at| at + CONFIG_MARKER.len())
        .ok_or("refused: its BOOTX64.EFI is not Limine")?;
    if efi.get(at..at + 128) != Some(blake2b(config).as_bytes()) {
        return Err("refused: its BOOTX64.EFI is not enrolled with its limine.conf".into());
    }
    Ok(())
}

/// Writes a whole file `name` into `dir`.
fn put<D: oceans_fat::Disk>(
    fat: &mut Fat<D>,
    dir: NodeId,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let file = fat.create(dir, name, false).map_err(fat_error(name))?;
    fat.retain(file).map_err(fat_error(name))?;
    let mut done = 0;
    let result = loop {
        if done == bytes.len() {
            break Ok(());
        }
        match fat.write_file(file, done as u64, &bytes[done..]) {
            Ok(0) => break Err(format!("{name}: the slot is full")),
            Ok(wrote) => done += wrote,
            Err(error) => break Err(fat_error(name)(error)),
        }
    };
    fat.release(file);
    result
}

/// A directory `name` in `dir`, held (the caller releases it).
fn directory<D: oceans_fat::Disk>(
    fat: &mut Fat<D>,
    dir: NodeId,
    name: &str,
) -> Result<NodeId, String> {
    let node = fat.create(dir, name, true).map_err(fat_error(name))?;
    fat.retain(node).map_err(fat_error(name))?;
    Ok(node)
}

/// What an update brings: its release and files.
pub struct Update<'a> {
    pub release: &'a str,
    pub kernel: &'a [u8],
    pub initrd: &'a [u8],
    pub efi: Option<&'a [u8]>,
    pub config: Option<&'a [u8]>,
}

/// Installs `update` into the slot not starting first, then makes it
/// start first (module docs). `running` is the running release.
pub fn apply(
    out: &mut Out,
    block: Handle,
    running: &str,
    update: &Update<'_>,
) -> Result<(), String> {
    let (Some(efi), Some(config)) = (update.efi, update.config) else {
        return Err(format!(
            "refused: this system boots with Secure Boot (ADR-0092), and the update has no \
             signed boot files ({UPDATE_EFI}, {UPDATE_CONFIG})"
        ));
    };
    check_boot_files(efi, config, update.kernel, update.initrd)?;
    let mut stick = Stick::open(block)?;
    if !stick.writable {
        return Err("the stick is read-only".into());
    }
    repair(out, &mut stick)?;
    let [first, second] = slots(&mut stick)?;
    // The running release must not be overwritten: if it is the previous
    // (chosen in the boot menu), it starts first from now on.
    if second.release.as_deref() == Some(running) && first.release.as_deref() != Some(running) {
        oceans_gpt::swap(&mut stick, FIRST, SECOND).map_err(gpt_error)?;
        let _ = writeln!(
            out,
            "update: slot {} (running) starts first again before the other is written",
            second.name
        );
    } else if first.release.as_deref() != Some(running) {
        return Err(format!(
            "the running release ({running}) is in neither slot"
        ));
    }
    let [_, target] = slots(&mut stick)?;
    let _ = writeln!(out, "update: writing slot {}", target.name);
    let _ = out.flush();

    // 1. The slot not starting first, made again from nothing: its label
    //    kept, its release written last.
    {
        let label = format!("OCEANS-{}", target.name);
        let mut window = Window::new(
            &mut stick,
            target.partition.start(),
            target.partition.size(),
        )
        .ok_or("a slot lies outside the stick")?;
        oceans_fat::format(&mut window, &label, oceans_gpt::crc32(label.as_bytes()))
            .map_err(fat_error("formatting the slot"))?;
        let mut fat = Fat::open(window).map_err(fat_error("the new slot"))?;
        fat.enable_writes().map_err(fat_error("the new slot"))?;
        let efi_dir = directory(&mut fat, ROOT, EFI_DIR[0])?;
        let boot_dir = directory(&mut fat, efi_dir, EFI_DIR[1])?;
        put(&mut fat, boot_dir, EFI_NAME, efi)?;
        fat.release(boot_dir);
        fat.release(efi_dir);
        put(&mut fat, ROOT, CONFIG, config)?;
        let boot = directory(&mut fat, ROOT, "boot")?;
        put(&mut fat, boot, KERNEL, update.kernel)?;
        put(&mut fat, boot, INITRD, update.initrd)?;
        fat.release(boot);
        fat.sync().map_err(fat_error("the slot"))?;
        put(
            &mut fat,
            ROOT,
            RELEASE,
            format!("{}\n", update.release).as_bytes(),
        )?;
        fat.sync().map_err(fat_error("the slot"))?;
    }

    // 2. The switch: the new slot starts first, the running one is the
    //    previous.
    oceans_gpt::swap(&mut stick, FIRST, SECOND).map_err(gpt_error)?;
    let _ = writeln!(
        out,
        "update: Oceans {} installed in slot {}; it starts at the next boot (the previous, \
         {running}, stays in the boot menu)",
        update.release, target.name
    );
    Ok(())
}
