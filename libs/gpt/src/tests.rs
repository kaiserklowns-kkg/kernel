use std::string::ToString;
use std::vec::Vec;

use super::*;

/// A disk image that records its writes, barrier by barrier.
#[derive(Clone)]
struct Image {
    bytes: Vec<u8>,
    /// Each write: (offset, data); `None` marks a barrier.
    log: Vec<Option<(u64, Vec<u8>)>>,
}

impl Disk for Image {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<(), IoError> {
        let at = offset as usize;
        out.copy_from_slice(self.bytes.get(at..at + out.len()).ok_or(IoError)?);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<(), IoError> {
        assert!(offset.is_multiple_of(SECTOR) && data.len().is_multiple_of(SECTOR as usize));
        let at = offset as usize;
        self.bytes
            .get_mut(at..at + data.len())
            .ok_or(IoError)?
            .copy_from_slice(data);
        self.log.push(Some((offset, data.to_vec())));
        Ok(())
    }

    fn flush(&mut self) -> Result<(), IoError> {
        self.log.push(None);
        Ok(())
    }

    fn writable(&self) -> bool {
        true
    }

    fn size(&self) -> u64 {
        self.bytes.len() as u64
    }
}

const SECTORS: u64 = 4096;

fn slot(name: &str, first: u64, guid: u8) -> Partition {
    Partition {
        kind: OCEANS_SLOT,
        guid: [guid; 16],
        first_lba: first,
        last_lba: first + 999,
        name: name.to_string(),
    }
}

fn disk() -> Image {
    let esp = Partition {
        kind: EFI_SYSTEM,
        guid: [1; 16],
        first_lba: 40,
        last_lba: 1039,
        name: "EFI system partition".to_string(),
    };
    let mut bytes = vec![0u8; (SECTORS * SECTOR) as usize];
    for (lba, data) in create(
        SECTORS,
        [9; 16],
        &[esp, slot("A", 1040, 2), slot("B", 2040, 3)],
    ) {
        let at = (lba * SECTOR) as usize;
        bytes[at..at + data.len()].copy_from_slice(&data);
    }
    Image {
        bytes,
        log: Vec::new(),
    }
}

/// The entries' starts, as a reader that trusts the primary blindly (no
/// CRC check) sees them.
fn blind_order(image: &Image) -> Vec<u64> {
    let entries = &image.bytes[2 * SECTOR as usize..2 * SECTOR as usize + 3 * ENTRY_SIZE];
    entries
        .as_chunks::<ENTRY_SIZE>()
        .0
        .iter()
        .map(|raw| u64_at(raw, 32))
        .collect()
}

fn order(image: &mut Image) -> Vec<u64> {
    read(image)
        .unwrap()
        .into_iter()
        .take(3)
        .map(|p| p.unwrap().first_lba)
        .collect()
}

#[test]
fn creates_a_table_both_copies_agree_on() {
    let mut image = disk();
    let parts = read(&mut image).unwrap();
    assert_eq!(parts[0].as_ref().unwrap().kind, EFI_SYSTEM);
    assert_eq!(parts[1].as_ref().unwrap().name, "A");
    assert_eq!(parts[2].as_ref().unwrap().size(), 1000 * SECTOR);
    assert!(parts[3].is_none());
    assert_eq!(repair(&mut image).unwrap(), Repair::Nothing);
    assert_eq!(usable(SECTORS), (34, SECTORS - 34));
}

#[test]
fn crc32_is_ieee() {
    assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
}

#[test]
fn swaps_two_entries() {
    let mut image = disk();
    swap(&mut image, 2, 3).unwrap();
    assert_eq!(order(&mut image), [40, 2040, 1040]);
    assert_eq!(read(&mut image).unwrap()[1].as_ref().unwrap().name, "B");
    assert_eq!(repair(&mut image).unwrap(), Repair::Nothing);
    swap(&mut image, 3, 2).unwrap();
    assert_eq!(order(&mut image), [40, 1040, 2040]);
    assert_eq!(swap(&mut image, 2, 2), Err(Error::NoSuchEntry));
    assert_eq!(swap(&mut image, 0, 2), Err(Error::NoSuchEntry));
    // Entries 4 and 5 are in different sectors: not one write.
    assert_eq!(swap(&mut image, 4, 5), Err(Error::Unsupported));
}

#[test]
fn every_power_cut_leaves_one_order_every_reader_agrees_on() {
    let base = disk();
    let mut done = base.clone();
    swap(&mut done, 2, 3).unwrap();
    let writes: Vec<(u64, Vec<u8>)> = done.log.iter().flatten().cloned().collect();
    // Each write is followed by a barrier: a cut leaves a prefix.
    assert!(done.log.iter().skip(1).step_by(2).all(Option::is_none));
    let old = vec![40, 1040, 2040];
    let new = vec![40, 2040, 1040];
    let mut seen_new_at = None;
    for cut in 0..=writes.len() {
        let mut image = base.clone();
        for (offset, data) in &writes[..cut] {
            image.write_at(*offset, data).unwrap();
        }
        let checked = order(&mut image);
        assert!(checked == old || checked == new, "cut {cut}: {checked:?}");
        // A reader that does not check CRCs sees the same.
        assert_eq!(blind_order(&image), checked, "cut {cut}");
        if checked == new && seen_new_at.is_none() {
            seen_new_at = Some(cut);
        }
        // Once new, always new.
        if let Some(at) = seen_new_at {
            assert!(cut < at || checked == new, "cut {cut} went back");
        }
        // Repair keeps what readers saw, and leaves both copies whole.
        repair(&mut image).unwrap();
        assert_eq!(
            order(&mut image),
            checked,
            "cut {cut}: repair changed the order"
        );
        assert_eq!(repair(&mut image).unwrap(), Repair::Nothing, "cut {cut}");
        assert_eq!(blind_order(&image), checked);
    }
    // The switch is the third write: the primary's entry sector.
    assert_eq!(seen_new_at, Some(3));
}

#[test]
fn a_damaged_primary_is_restored_from_the_backup() {
    let mut image = disk();
    image.bytes[SECTOR as usize + 30] ^= 1;
    assert_eq!(order(&mut image), [40, 1040, 2040]);
    assert_eq!(repair(&mut image).unwrap(), Repair::Primary);
    assert_eq!(repair(&mut image).unwrap(), Repair::Nothing);
    let mut blank = Image {
        bytes: vec![0u8; (SECTORS * SECTOR) as usize],
        log: Vec::new(),
    };
    assert_eq!(read(&mut blank), Err(Error::NotGpt));
}
