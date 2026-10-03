use super::*;

fn dword(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[4 * index..4 * index + 4].try_into().unwrap())
}

#[test]
fn capabilities_decode() {
    // MQES 2047, TO 20 (10 s), DSTRD 1, CSS NVM, MPSMIN 0, MPSMAX 4.
    let cap = 2047 | (20 << 24) | (1 << 32) | (1 << 37) | (4 << 52);
    let caps = Capabilities::decode(cap);
    assert_eq!(caps.max_queue_entries, 2048);
    assert_eq!(caps.timeout_ms, 10_000);
    assert_eq!(caps.doorbell_stride, 8);
    assert!(caps.nvm_command_set);
    assert!(caps.supports_4k_pages());
    assert_eq!(caps.submission_doorbell(0), 0x1000);
    assert_eq!(caps.completion_doorbell(0), 0x1008);
    assert_eq!(caps.submission_doorbell(1), 0x1010);
    assert_eq!(caps.completion_doorbell(1), 0x1018);
    // Only 8 KiB pages and up.
    assert!(!Capabilities::decode(1 << 48).supports_4k_pages());
    assert_eq!(version(0x0001_0400), (1, 4));
}

#[test]
fn commands_encode() {
    let mut read = Command::read(1, 0x1_2345_6789, 8, (0x10_0000, 0x20_0000));
    read.id = 0xbeef;
    let bytes = read.encode();
    assert_eq!(dword(&bytes, 0), 0xbeef_0002);
    assert_eq!(dword(&bytes, 1), 1);
    assert_eq!(
        u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
        0x10_0000
    );
    assert_eq!(
        u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
        0x20_0000
    );
    assert_eq!(dword(&bytes, 10), 0x2345_6789);
    assert_eq!(dword(&bytes, 11), 1);
    assert_eq!(dword(&bytes, 12), 7, "the block count is zero-based");

    let cq = Command::create_completion_queue(1, 16, 0x5000, Some(0)).encode();
    assert_eq!(bytes_opcode(&cq), opcode::CREATE_IO_CQ);
    assert_eq!(dword(&cq, 10), (15 << 16) | 1);
    assert_eq!(dword(&cq, 11), 0b11, "interrupts on vector 0, contiguous");
    let polled = Command::create_completion_queue(1, 16, 0x5000, None).encode();
    assert_eq!(dword(&polled, 11), 0b01);

    let sq = Command::create_submission_queue(1, 16, 0x6000, 1).encode();
    assert_eq!(bytes_opcode(&sq), opcode::CREATE_IO_SQ);
    assert_eq!(dword(&sq, 11), (1 << 16) | 1);

    let identify = Command::identify(cns::CONTROLLER, 0, 0x7000).encode();
    assert_eq!(bytes_opcode(&identify), opcode::IDENTIFY);
    assert_eq!(dword(&identify, 10), 1);

    let queues = Command::set_queue_count(1).encode();
    assert_eq!(dword(&queues, 10), 7);
    assert_eq!(dword(&queues, 11), 0);

    let flush = Command::flush(1).encode();
    assert_eq!(bytes_opcode(&flush), opcode::FLUSH);
    assert_eq!(dword(&flush, 1), 1);
}

fn bytes_opcode(bytes: &[u8; COMMAND_SIZE]) -> u8 {
    bytes[0]
}

#[test]
fn completions_decode() {
    let mut entry = [0u8; COMPLETION_SIZE];
    entry[0..4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    entry[8..12].copy_from_slice(&((1u32 << 16) | 5).to_le_bytes());
    entry[12..16].copy_from_slice(&(0x1234u32 | (1 << 16)).to_le_bytes());
    let done = Completion::decode(&entry);
    assert_eq!(done.result, 0xdead_beef);
    assert_eq!((done.sq_id, done.sq_head, done.id), (1, 5, 0x1234));
    assert!(done.phase);
    assert!(done.status.is_success());

    // LBA out of range (generic 0x80), do not retry.
    let status: u32 = 0x80 | (1 << 14);
    entry[12..16].copy_from_slice(&(7 | (status << 17)).to_le_bytes());
    let failed = Completion::decode(&entry);
    assert!(!failed.phase);
    assert!(!failed.status.is_success());
    assert!(failed.status.do_not_retry());
    assert_eq!(failed.status.message(), "LBA out of range");
    // An unrecovered read error (media, type 2).
    let media = StatusCode((2 << 8) | 0x81);
    assert_eq!(media.message(), "unrecovered read error");
}

fn controller_data() -> [u8; IDENTIFY_SIZE] {
    let mut data = [0u8; IDENTIFY_SIZE];
    data[4..24].copy_from_slice(b"oceans-nvme         ");
    data[24..64].copy_from_slice(b"QEMU NVMe Ctrl                          ");
    data[64..72].copy_from_slice(b"9.1.0   ");
    data[77] = 5;
    data[516..520].copy_from_slice(&256u32.to_le_bytes());
    data[525] = 1;
    data
}

#[test]
fn controller_identify() {
    let info = ControllerInfo::parse(&controller_data()).unwrap();
    assert_eq!(info.serial(), "oceans-nvme");
    assert_eq!(info.model(), "QEMU NVMe Ctrl");
    assert_eq!(info.firmware(), "9.1.0");
    assert_eq!(info.namespaces, 256);
    assert!(info.volatile_write_cache);
    assert_eq!(info.max_transfer(12), Some(128 * 1024));
    let mut unlimited = controller_data();
    unlimited[77] = 0;
    assert_eq!(
        ControllerInfo::parse(&unlimited).unwrap().max_transfer(12),
        None
    );
    // Not ASCII: shown as `?`, never as garbage.
    let mut odd = controller_data();
    odd[24] = 0xff;
    assert_eq!(ControllerInfo::parse(&odd).unwrap().model(), "?");
    assert!(ControllerInfo::parse(&[0u8; 100]).is_none());
}

fn namespace_data(format: u8, lbads: u8, metadata: u16) -> [u8; IDENTIFY_SIZE] {
    let mut data = [0u8; IDENTIFY_SIZE];
    data[0..8].copy_from_slice(&65_536u64.to_le_bytes());
    data[25] = 1; // two formats
    data[26] = format;
    data[128 + 2] = 9;
    data[132..134].copy_from_slice(&metadata.to_le_bytes());
    data[132 + 2] = lbads;
    data
}

#[test]
fn namespace_identify() {
    let first = NamespaceInfo::parse(&namespace_data(0, 12, 0)).unwrap();
    assert_eq!(
        (first.blocks, first.block_size, first.metadata_size),
        (65_536, 512, 0)
    );
    assert!(!first.write_protected);
    let second = NamespaceInfo::parse(&namespace_data(1, 12, 8)).unwrap();
    assert_eq!((second.block_size, second.metadata_size), (4096, 8));
    // A format index past the ones listed, or a block size below 512.
    assert!(NamespaceInfo::parse(&namespace_data(2, 12, 0)).is_none());
    assert!(NamespaceInfo::parse(&namespace_data(1, 8, 0)).is_none());
    let mut protected = namespace_data(0, 9, 0);
    protected[99] = 1;
    assert!(NamespaceInfo::parse(&protected).unwrap().write_protected);

    let mut list = [0u8; IDENTIFY_SIZE];
    assert_eq!(first_namespace(&list), None);
    list[0..4].copy_from_slice(&3u32.to_le_bytes());
    list[4..8].copy_from_slice(&7u32.to_le_bytes());
    assert_eq!(first_namespace(&list), Some(3));
}

#[test]
fn prps_describe_contiguous_buffers() {
    let base = 0x100_0000;
    assert_eq!(prp(base, 512, 0x9000), (base, 0));
    assert_eq!(prp(base, PAGE_SIZE, 0x9000), (base, 0));
    assert_eq!(prp(base, PAGE_SIZE + 1, 0x9000), (base, base + 0x1000));
    assert_eq!(prp(base, 3 * PAGE_SIZE, 0x9000), (base, 0x9000));
    assert_eq!(prp_list_entry(base, 0), base + 0x1000);
    assert_eq!(prp_list_entry(base, 14), base + 0xf000);
}

#[test]
fn chunks_with_512_byte_blocks() {
    // 64 KiB from sector 10, 32 blocks a time: two whole transfers.
    let (start, end) = (10 * 512, 10 * 512 + 65_536);
    let first = next_chunk(start, end, 512, 32);
    assert_eq!(
        first,
        Chunk {
            lba: 10,
            blocks: 32,
            offset: 0,
            len: 16_384,
            partial: false
        }
    );
    let second = next_chunk(start + 16_384, end, 512, 128);
    assert_eq!((second.lba, second.blocks, second.len), (42, 96, 49_152));
    assert!(!second.partial);
}

#[test]
fn chunks_with_4k_blocks_cover_whole_blocks() {
    // One sector in the middle of a block: the whole block, partial.
    let one = next_chunk(3 * 512, 4 * 512, 4096, 16);
    assert_eq!(
        one,
        Chunk {
            lba: 0,
            blocks: 1,
            offset: 1536,
            len: 512,
            partial: true
        }
    );
    // Sectors 7..17 cross into the third block.
    let across = next_chunk(7 * 512, 17 * 512, 4096, 16);
    assert_eq!(
        (across.lba, across.blocks, across.offset, across.len),
        (0, 3, 3584, 5120)
    );
    assert!(across.partial);
    // Aligned on both ends: no read needed before writing.
    let aligned = next_chunk(8 * 512, 24 * 512, 4096, 16);
    assert_eq!((aligned.lba, aligned.blocks, aligned.offset), (1, 2, 0));
    assert!(!aligned.partial);
    // Limited by the transfer size, the remainder follows.
    let limited = next_chunk(512, 64 * 512, 4096, 2);
    assert_eq!(
        (limited.lba, limited.blocks, limited.len),
        (0, 2, 8192 - 512)
    );
    let rest = next_chunk(512 + limited.len as u64, 64 * 512, 4096, 2);
    assert_eq!((rest.lba, rest.offset), (2, 0));
}

#[test]
fn every_chunk_sequence_covers_the_request_exactly() {
    for block_size in [512u32, 1024, 4096] {
        for max_blocks in [1u32, 3, 16] {
            for first in 0..20u64 {
                for count in 1..40u64 {
                    let (mut position, end) = (first * 512, (first + count) * 512);
                    while position < end {
                        let chunk = next_chunk(position, end, block_size, max_blocks);
                        let size = u64::from(block_size);
                        assert!(chunk.blocks >= 1 && chunk.blocks <= max_blocks);
                        assert_eq!(chunk.lba * size + chunk.offset as u64, position);
                        assert!(chunk.offset + chunk.len <= (chunk.blocks as u64 * size) as usize);
                        assert!(chunk.len > 0);
                        position += chunk.len as u64;
                    }
                    assert_eq!(position, end);
                }
            }
        }
    }
}

#[test]
fn rings_flip_phase_on_wrap() {
    let mut ring = Ring::new(3);
    assert!(ring.phase);
    ring.advance();
    ring.advance();
    assert_eq!((ring.index, ring.phase), (2, true));
    ring.advance();
    assert_eq!((ring.index, ring.phase), (0, false));
}
