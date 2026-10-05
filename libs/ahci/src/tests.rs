extern crate std;

use super::*;

fn dword(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[4 * index..4 * index + 4].try_into().unwrap())
}

#[test]
fn capabilities_decode() {
    // QEMU's ICH9: 6 ports, 32 slots, 64-bit, AHCI only.
    let cap = 5 | (31 << 8) | (1 << 18) | (1 << 31);
    let caps = Capabilities::decode(cap);
    assert_eq!(caps.ports, 6);
    assert_eq!(caps.slots, 32);
    assert!(caps.addr64 && caps.ahci_only && !caps.staggered_spin_up);
    assert!(caps.reaches(0x1_0000_0000, 4096));
    // A 32-bit controller reaches only the first 4 GiB.
    let narrow = Capabilities::decode(1 << 27);
    assert!(narrow.staggered_spin_up && !narrow.addr64);
    assert!(narrow.reaches(0xffff_f000, 4096));
    assert!(!narrow.reaches(0xffff_f000, 4097));
    assert!(!narrow.reaches(u64::MAX, 1));
    assert_eq!(version(0x0001_0301), (1, 3, 1));
    assert_eq!(version(0x0001_0000), (1, 0, 0));
}

#[test]
fn ports_and_registers() {
    assert_eq!(
        implemented_ports(0b10_0101).collect::<std::vec::Vec<_>>(),
        [0, 2, 5]
    );
    assert_eq!(implemented_ports(0).count(), 0);
    assert_eq!(implemented_ports(u32::MAX).count(), 32);
    assert_eq!(hba::port(0), 0x100);
    assert_eq!(hba::port(5) + port::CI, 0x100 + 5 * 0x80 + 0x38);
    assert_eq!(hba::port(31) + hba::PORT_SIZE, 0x1100);
}

#[test]
fn layout_alignments_hold() {
    use layout::*;
    assert!(COMMAND_LIST.is_multiple_of(1024));
    assert!(COMMAND_LIST + COMMAND_LIST_SIZE <= RECEIVED_FIS);
    assert!(RECEIVED_FIS.is_multiple_of(256));
    assert!(RECEIVED_FIS + RECEIVED_FIS_SIZE <= COMMAND_TABLE);
    assert!(COMMAND_TABLE.is_multiple_of(128));
    assert_eq!(PRDT - COMMAND_TABLE, 0x80);
    assert!(PRDT + MAX_PRDS * PRD_SIZE <= SIZE);
    assert_eq!(MAX_PRDS, 120);
}

#[test]
fn link_status_decodes() {
    // QEMU with a disk: present and talking, generation 1, active.
    let up = LinkStatus::decode(0x113);
    assert!(up.is_up() && !up.is_empty());
    assert_eq!(up.speed_name(), "1.5 Gb/s");
    // In slumber: not up.
    assert!(!LinkStatus::decode(0x633).is_up());
    // Present, no communication yet.
    let waking = LinkStatus::decode(0x001);
    assert!(!waking.is_up() && !waking.is_empty());
    assert!(LinkStatus::decode(0).is_empty());
    assert_eq!(LinkStatus::decode(0x133).speed_name(), "6 Gb/s");
}

#[test]
fn task_file_decodes() {
    let idle = TaskFile::decode(0x0050);
    assert!(!idle.is_busy() && !idle.has_error());
    assert_eq!(idle.message(), "no device error");
    assert!(TaskFile::decode(0x80).is_busy());
    assert!(TaskFile::decode(0x58).is_busy());
    // Error with IDNF: out of range.
    let range = TaskFile::decode(0x1051);
    assert!(range.has_error() && range.out_of_range());
    assert_eq!(range.message(), "sector not found");
    let aborted = TaskFile::decode(0x0451);
    assert!(!aborted.out_of_range());
    assert_eq!(aborted.message(), "command aborted");
    assert_eq!(
        TaskFile::decode(0x4051).message(),
        "uncorrectable data error"
    );
    assert_eq!(TaskFile::decode(0x8051).message(), "interface CRC error");
    // Error bits without the status ERR bit mean nothing.
    assert!(!TaskFile::decode(0x1050).out_of_range());
    assert_eq!(port_error_message(is::TASK_FILE_ERROR), "task file error");
    assert_eq!(
        port_error_message(is::HOST_BUS_FATAL | is::TASK_FILE_ERROR),
        "host bus fatal error"
    );
    assert_eq!(port_error_message(is::D2H_REGISTER), "no port error");
}

#[test]
fn signatures_name_devices() {
    assert_eq!(sig::name(0x101), "a SATA disk");
    assert_eq!(sig::name(0xeb14_0101), "an ATAPI device");
    assert_eq!(sig::name(u32::MAX), "no signature yet");
    assert_eq!(sig::name(0x1234), "an unknown device");
}

#[test]
fn commands_encode_as_fises() {
    let read = AtaCommand::read(0x0000_1234_5678_9abc, 256);
    let fis = read.fis();
    assert_eq!(fis[0], 0x27);
    assert_eq!(fis[1], 0x80);
    assert_eq!(fis[2], 0x25);
    assert_eq!(fis[3], 0);
    assert_eq!(&fis[4..7], &[0xbc, 0x9a, 0x78]);
    assert_eq!(fis[7], 0x40);
    assert_eq!(&fis[8..11], &[0x56, 0x34, 0x12]);
    assert_eq!(fis[11], 0);
    assert_eq!(&fis[12..14], &[0x00, 0x01]);
    assert_eq!(&fis[14..], &[0; 6]);
    assert!(!read.writes());
    assert_eq!(read.data_len(), 256 * 512);

    let write = AtaCommand::write(7, 1);
    assert_eq!(write.fis()[2], 0x35);
    assert!(write.writes());
    assert_eq!(write.data_len(), 512);

    let identify = AtaCommand::identify();
    assert_eq!(identify.fis()[2], 0xec);
    assert_eq!(identify.fis()[7], 0);
    assert_eq!(identify.data_len(), 512);
    assert!(!identify.writes());

    assert_eq!(AtaCommand::flush(true).fis()[2], 0xea);
    assert_eq!(AtaCommand::flush(false).fis()[2], 0xe7);
    assert_eq!(AtaCommand::flush(true).data_len(), 0);
}

#[test]
fn headers_and_prds_encode() {
    let header = command_header(true, 1, 0x1234_5000_0880);
    // CFL 5 dwords, W, one PRD.
    assert_eq!(dword(&header, 0), 5 | (1 << 6) | (1 << 16));
    assert_eq!(dword(&header, 1), 0);
    assert_eq!(dword(&header, 2), 0x5000_0880);
    assert_eq!(dword(&header, 3), 0x1234);
    assert_eq!(&header[16..], &[0; 16]);
    assert_eq!(dword(&command_header(false, 0, 0x80), 0), 5);

    let entry = prd(0x8000_0000, 128 * 1024, 0).unwrap();
    assert_eq!(dword(&entry, 0), 0x8000_0000);
    assert_eq!(dword(&entry, 1), 0);
    assert_eq!(dword(&entry, 3), 128 * 1024 - 1);
    assert!(prd(0x8000_0000, 128 * 1024, 1).is_none());
    assert_eq!(prd_count(128 * 1024), 1);
    assert_eq!(prd_count(0), 0);
}

/// Every split of a buffer into PRD entries covers it exactly, in order,
/// within the 22-bit count.
#[test]
fn prds_cover_buffers_exactly() {
    let base = 0x1_0000_0000u64;
    for len in [
        512,
        4096,
        MAX_PRD_BYTES,
        MAX_PRD_BYTES + 512,
        3 * MAX_PRD_BYTES + 1024,
    ] {
        let mut next = base;
        let mut count = 0;
        while let Some(entry) = prd(base, len, count) {
            let address = u64::from(dword(&entry, 0)) | (u64::from(dword(&entry, 1)) << 32);
            let bytes = dword(&entry, 3) as usize + 1;
            assert_eq!(address, next);
            assert!(bytes <= MAX_PRD_BYTES && bytes.is_multiple_of(2));
            next += bytes as u64;
            count += 1;
        }
        assert_eq!(next - base, len as u64);
        assert_eq!(count, prd_count(len));
    }
    assert!(prd(0, 512, usize::MAX).is_none());
}

/// IDENTIFY DEVICE data as QEMU's `ide-hd` reports it.
fn identify_data(sectors: u64) -> [u8; IDENTIFY_SIZE] {
    let mut data = [0u8; IDENTIFY_SIZE];
    let mut put = |word: usize, value: u16| {
        data[2 * word..2 * word + 2].copy_from_slice(&value.to_le_bytes())
    };
    put(0, 0x0040);
    put(60, sectors.min(0x0fff_ffff) as u16);
    put(61, (sectors.min(0x0fff_ffff) >> 16) as u16);
    put(82, (1 << 14) | (1 << 5) | 1);
    put(83, (1 << 14) | (1 << 13) | (1 << 12) | (1 << 10));
    put(85, (1 << 14) | (1 << 5) | 1);
    put(87, 1 << 14);
    for i in 0..4 {
        put(100 + i, (sectors >> (16 * i)) as u16);
    }
    put(106, 0x6000);
    // Strings: the first character of each pair in the high byte.
    let mut string = |at: usize, text: &[u8], len: usize| {
        let mut padded = std::vec![b' '; len];
        padded[..text.len()].copy_from_slice(text);
        for (i, pair) in padded.chunks(2).enumerate() {
            data[at + 2 * i] = pair[1];
            data[at + 2 * i + 1] = pair[0];
        }
    };
    string(20, b"QM00001", 20);
    string(46, b"2.5+", 8);
    string(54, b"QEMU HARDDISK", 40);
    data
}

fn set_word(data: &mut [u8], word: usize, value: u16) {
    data[2 * word..2 * word + 2].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn identify_parses() {
    let id = Identity::parse(&identify_data(32768)).unwrap();
    assert_eq!(id.model(), "QEMU HARDDISK");
    assert_eq!(id.serial(), "QM00001");
    assert_eq!(id.firmware(), "2.5+");
    assert!(id.ata && id.lba48 && id.write_cache && id.flush_ext);
    assert_eq!(id.sectors, 32768);
    assert_eq!(id.logical_sector_size, 512);
    assert_eq!(id.physical_sector_size, 512);
    assert!(Identity::parse(&[0u8; 100]).is_none());

    // A disk beyond 28-bit addressing: the 48-bit count.
    let big = Identity::parse(&identify_data(1 << 40)).unwrap();
    assert_eq!(big.sectors, 1 << 40);

    // An ATAPI device.
    let mut packet = identify_data(0);
    set_word(&mut packet, 0, 0x85c0);
    assert!(!Identity::parse(&packet).unwrap().ata);

    // No LBA48: the 28-bit count.
    let mut old = identify_data(1000);
    set_word(&mut old, 83, 1 << 14);
    let old = Identity::parse(&old).unwrap();
    assert!(!old.lba48 && !old.flush_ext);
    assert_eq!(old.sectors, 1000);

    // Words 82..87 not valid: no features at all.
    let mut invalid = identify_data(1000);
    set_word(&mut invalid, 83, 0xffff);
    set_word(&mut invalid, 87, 0);
    let invalid = Identity::parse(&invalid).unwrap();
    assert!(!invalid.lba48 && !invalid.write_cache);

    // Write cache present but off: no flush needed.
    let mut off = identify_data(1000);
    set_word(&mut off, 85, (1 << 14) | 1);
    assert!(!Identity::parse(&off).unwrap().write_cache);

    // A device reporting more than 48 bits is clamped.
    let mut absurd = identify_data(0);
    set_word(&mut absurd, 103, 0xffff);
    assert_eq!(Identity::parse(&absurd).unwrap().sectors, LBA48_LIMIT);
}

#[test]
fn sector_sizes_parse() {
    // 4Kn: 4096-byte logical sectors (2048 words).
    let mut native = identify_data(1000);
    set_word(&mut native, 106, 0x5000);
    set_word(&mut native, 117, 2048);
    let native = Identity::parse(&native).unwrap();
    assert_eq!(native.logical_sector_size, 4096);
    assert_eq!(native.physical_sector_size, 4096);
    // 512e: 512-byte logical, 4096-byte physical (2^3 per physical).
    let mut emulated = identify_data(1000);
    set_word(&mut emulated, 106, 0x6003);
    let emulated = Identity::parse(&emulated).unwrap();
    assert_eq!(emulated.logical_sector_size, 512);
    assert_eq!(emulated.physical_sector_size, 4096);
    // Word 106 not valid: 512 bytes.
    let mut invalid = identify_data(1000);
    set_word(&mut invalid, 106, 0x1000 | 0x8000);
    set_word(&mut invalid, 117, 2048);
    assert_eq!(Identity::parse(&invalid).unwrap().logical_sector_size, 512);
}

#[test]
fn identify_strings_are_checked() {
    let mut odd = identify_data(1000);
    odd[55] = 0xff;
    assert_eq!(Identity::parse(&odd).unwrap().model(), "?");
    // All padding: empty.
    let mut blank = identify_data(1000);
    blank[20..40].fill(b' ');
    assert_eq!(Identity::parse(&blank).unwrap().serial(), "");
    // Leading spaces (some drives right-align serials) are dropped.
    let mut right = identify_data(1000);
    right[20..40].copy_from_slice(b"                1Z23");
    assert_eq!(Identity::parse(&right).unwrap().serial(), "Z132");
}

/// Splitting a request covers it exactly, within the bounce buffer and the
/// 16-bit sector count.
#[test]
fn chunks_cover_requests() {
    for (count, max) in [
        (1, 256),
        (256, 256),
        (257, 256),
        (1000, 7),
        (200_000, 200_000),
    ] {
        let mut remaining: u32 = count;
        let mut moved = 0u32;
        while remaining > 0 {
            let n = chunk_sectors(remaining, max);
            assert!(n > 0 && u32::from(n) <= max && u32::from(n) <= MAX_COMMAND_SECTORS);
            moved += u32::from(n);
            remaining -= u32::from(n);
        }
        assert_eq!(moved, count);
    }
}
