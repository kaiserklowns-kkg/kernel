use std::collections::BTreeMap;
use std::vec::Vec;

use oceans_fat::{Disk, Fat, IoError, ROOT, Window};

use super::*;

/// A sparse disk in memory: sectors never written read as zeros.
struct Sparse {
    sectors: BTreeMap<u64, [u8; 512]>,
    size: u64,
}

impl Sparse {
    fn new(bytes: u64) -> Self {
        Self {
            sectors: BTreeMap::new(),
            size: bytes,
        }
    }
}

impl Disk for Sparse {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<(), IoError> {
        if offset + out.len() as u64 > self.size {
            return Err(IoError);
        }
        for (i, byte) in out.iter_mut().enumerate() {
            let at = offset + i as u64;
            *byte = self
                .sectors
                .get(&(at / 512))
                .map_or(0, |s| s[(at % 512) as usize]);
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), IoError> {
        if offset + data.len() as u64 > self.size {
            return Err(IoError);
        }
        for (i, &byte) in data.iter().enumerate() {
            let at = offset + i as u64;
            self.sectors.entry(at / 512).or_insert([0; 512])[(at % 512) as usize] = byte;
        }
        Ok(())
    }

    fn writable(&self) -> bool {
        true
    }

    fn size(&self) -> u64 {
        self.size
    }
}

const GUIDS: Guids = Guids {
    disk: [1; 16],
    esp: [2; 16],
    root: [3; 16],
};
const GIB: u64 = 1 << 30;

#[test]
fn plans_an_esp_then_the_root() {
    let sectors = 4 * GIB / 512;
    let plan = plan(sectors, GUIDS).unwrap();
    assert_eq!(plan.esp.first_lba, 2048);
    assert_eq!(plan.esp.size(), ESP_MIB << 20);
    assert_eq!(plan.root.first_lba, plan.esp.last_lba + 1);
    assert_eq!((plan.root.last_lba + 1) % ALIGN, 0);
    // Before the backup table.
    assert!(plan.root.last_lba < oceans_gpt::usable(sectors).1);
    assert_eq!((plan.esp.guid, plan.root.guid), ([2; 16], [3; 16]));
    assert_eq!(plan.root.kind, OCEANS_ROOT);
    // Too small for a root of 1 GiB.
    assert_eq!(super::plan(GIB / 512, GUIDS), Err(Error::TooSmall));
    assert_eq!(super::plan(0, GUIDS), Err(Error::TooSmall));
}

#[test]
fn the_configuration_starts_slot_a() {
    let conf = boot_configuration("0.1.0 alpha");
    assert!(conf.contains("timeout: 3\n"));
    let first = conf.find("/Oceans 0.1.0 alpha\n").unwrap();
    let safe = conf.find("/Oceans 0.1.0 alpha (safe mode)\n").unwrap();
    assert!(first < safe);
    assert!(conf.contains("    path: boot():/boot/a/oceans-kernel\n"));
    assert!(conf.contains("    cmdline: oceans.mode=recovery\n"));
    assert_eq!(
        conf.matches("module_path: boot():/boot/a/initrd\n").count(),
        3
    );
}

fn read_all<D: Disk>(fat: &mut Fat<D>, path: &[&str]) -> Vec<u8> {
    let mut node = ROOT;
    for name in path {
        node = fat
            .lookup(node, name)
            .unwrap_or_else(|_| panic!("{path:?}"));
    }
    let mut out = vec![0u8; fat.size(node).unwrap() as usize];
    let mut done = 0;
    while done < out.len() {
        done += fat.read_file(node, done as u64, &mut out[done..]).unwrap();
    }
    out
}

#[test]
fn lays_out_a_disk_that_held_windows() {
    let size = 4 * GIB;
    let sectors = size / 512;
    let mut disk = Sparse::new(size);
    // What was there: another table, and data at the root's place.
    let windows = Partition {
        kind: [9; 16],
        guid: [9; 16],
        first_lba: 2048,
        last_lba: sectors - 4096,
        name: "Basic data partition".to_string(),
    };
    for (lba, bytes) in oceans_gpt::create(sectors, [8; 16], &[windows]) {
        disk.write_at(lba * 512, &bytes).unwrap();
    }
    let plan = plan(sectors, GUIDS).unwrap();
    disk.write_at(plan.root.start(), &[0x5a; 4096]).unwrap();

    let payload = Payload {
        limine: b"limine efi",
        kernel: &[7u8; 300_000],
        initrd: &[6u8; 1_000_000],
        release: "0.1.0 alpha",
    };
    let mut disk = lay_out(disk, sectors, GUIDS, &plan, &payload).unwrap();

    let partitions: Vec<Partition> = oceans_gpt::read(&mut disk)
        .unwrap()
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(partitions, [plan.esp.clone(), plan.root.clone()]);
    let mut start = [0u8; 4096];
    disk.read_at(plan.root.start(), &mut start).unwrap();
    assert_eq!(start, [0; 4096], "the root is blank");

    let window = Window::new(&mut disk, plan.esp.start(), plan.esp.size()).unwrap();
    let mut fat = Fat::open(window).unwrap();
    assert_eq!(
        read_all(&mut fat, &["EFI", "BOOT", "BOOTX64.EFI"]),
        b"limine efi"
    );
    assert_eq!(
        read_all(&mut fat, &["boot", "a", "oceans-kernel"]),
        [7u8; 300_000]
    );
    assert_eq!(
        read_all(&mut fat, &["boot", "a", "initrd"]).len(),
        1_000_000
    );
    assert_eq!(
        read_all(&mut fat, &["boot", "a", "release"]),
        b"0.1.0 alpha\n"
    );
    let conf = boot_configuration("0.1.0 alpha");
    assert_eq!(read_all(&mut fat, &["limine.conf"]), conf.as_bytes());
    assert_eq!(
        read_all(&mut fat, &["boot", "limine", "limine.conf"]),
        conf.as_bytes()
    );
}

#[test]
fn guids_as_gpt_stores_them() {
    // The Oceans root type, printed and stored.
    assert_eq!(
        parse_guid("0e5ea0b5-1c9b-4f4e-8a3d-0cea45051093"),
        Some(OCEANS_ROOT)
    );
    assert_eq!(parse_guid("0e5ea0b5-1c9b-4f4e-8a3d-0cea4505109"), None);
    assert_eq!(parse_guid("0e5ea0b5-1c9b-4f4e-8a3d-0cea4505109x"), None);
    assert_eq!(parse_guid("0e5ea0b51c9b-4f4e-8a3d-0cea45051093-"), None);
}

#[test]
fn the_staging_partition() {
    let guid = parse_guid("0e5ea0b5-1c9b-4f4e-8a3d-0cea45051093").unwrap();
    assert_eq!(format_guid(&guid), "0e5ea0b5-1c9b-4f4e-8a3d-0cea45051093");
    let order = install_order(&guid);
    assert_eq!(order, "esp = 0e5ea0b5-1c9b-4f4e-8a3d-0cea45051093\n");
    let conf = staging_configuration("0.1.0 alpha");
    assert!(conf.contains("timeout: 0\n"));
    assert!(conf.contains("    path: boot():/oceans/oceans-kernel\n"));
    assert!(conf.contains("    cmdline: oceans.mode=install\n"));
    let files = staged_files(b"opk", b"k", b"i", b"efi", &conf, &order);
    assert!(
        files
            .iter()
            .any(|&(path, bytes)| path == STAGED_PACKAGE && bytes == b"opk")
    );
    assert_eq!(
        files.iter().filter(|(_, bytes)| *bytes == b"efi").count(),
        2
    );
}
