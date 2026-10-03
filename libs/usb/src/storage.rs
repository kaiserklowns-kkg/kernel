//! USB mass storage, Bulk-Only Transport (BOT 1.0) with the SCSI
//! transparent command set (SPC-4, SBC-3 subsets): command and status
//! wrappers, the commands a block driver needs, and their replies.

use crate::request::Setup;

/// Interface class, subclass (SCSI transparent) and protocol (BOT).
pub const CLASS: u8 = 0x08;
pub const SUBCLASS_SCSI: u8 = 0x06;
pub const PROTOCOL_BOT: u8 = 0x50;

pub const CBW_SIZE: usize = 31;
pub const CSW_SIZE: usize = 13;
const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;

/// Data direction of a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    None,
    In,
    Out,
}

/// A SCSI command block (6 to 16 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    pub bytes: [u8; 16],
    pub len: u8,
    pub direction: Direction,
    /// Bytes the data stage moves.
    pub transfer: u32,
}

impl Command {
    fn new(bytes: &[u8], direction: Direction, transfer: u32) -> Self {
        let mut out = [0u8; 16];
        out[..bytes.len()].copy_from_slice(bytes);
        Self {
            bytes: out,
            len: bytes.len() as u8,
            direction,
            transfer,
        }
    }

    pub fn test_unit_ready() -> Self {
        Self::new(&[0x00, 0, 0, 0, 0, 0], Direction::None, 0)
    }

    pub fn request_sense() -> Self {
        Self::new(&[0x03, 0, 0, 0, 18, 0], Direction::In, 18)
    }

    pub fn inquiry() -> Self {
        Self::new(&[0x12, 0, 0, 0, 36, 0], Direction::In, 36)
    }

    pub fn read_capacity_10() -> Self {
        Self::new(&[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], Direction::In, 8)
    }

    pub fn read_capacity_16() -> Self {
        Self::new(
            &[0x9e, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32, 0, 0],
            Direction::In,
            32,
        )
    }

    /// READ(10), or READ(16) past 2^32 blocks or 65535 blocks at once.
    pub fn read(block: u64, count: u32, block_size: u32) -> Self {
        Self::rw(0x28, 0x88, block, count, block_size, Direction::In)
    }

    pub fn write(block: u64, count: u32, block_size: u32) -> Self {
        Self::rw(0x2a, 0x8a, block, count, block_size, Direction::Out)
    }

    fn rw(
        op10: u8,
        op16: u8,
        block: u64,
        count: u32,
        block_size: u32,
        direction: Direction,
    ) -> Self {
        let transfer = count.saturating_mul(block_size);
        if block <= u64::from(u32::MAX) && count <= u32::from(u16::MAX) {
            let b = (block as u32).to_be_bytes();
            let c = (count as u16).to_be_bytes();
            Self::new(
                &[op10, 0, b[0], b[1], b[2], b[3], 0, c[0], c[1], 0],
                direction,
                transfer,
            )
        } else {
            let mut bytes = [0u8; 16];
            bytes[0] = op16;
            bytes[2..10].copy_from_slice(&block.to_be_bytes());
            bytes[10..14].copy_from_slice(&count.to_be_bytes());
            Self::new(&bytes, direction, transfer)
        }
    }

    pub fn synchronize_cache() -> Self {
        Self::new(&[0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0], Direction::None, 0)
    }
}

/// The command block wrapper for `command`, tagged.
pub fn cbw(tag: u32, lun: u8, command: &Command) -> [u8; CBW_SIZE] {
    let mut out = [0u8; CBW_SIZE];
    out[..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
    out[4..8].copy_from_slice(&tag.to_le_bytes());
    out[8..12].copy_from_slice(&command.transfer.to_le_bytes());
    out[12] = if command.direction == Direction::In {
        0x80
    } else {
        0
    };
    out[13] = lun & 0x0f;
    out[14] = command.len;
    out[15..15 + usize::from(command.len)]
        .copy_from_slice(&command.bytes[..usize::from(command.len)]);
    out
}

/// A command status wrapper's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Passed {
        residue: u32,
    },
    Failed {
        residue: u32,
    },
    /// The device lost track: reset recovery is needed.
    PhaseError,
}

/// Checks a status wrapper: its size, signature and tag must match, and
/// the residue may not exceed what was asked (BOT §6.3). `None` means
/// the wrapper is invalid (reset recovery).
pub fn csw(bytes: &[u8], tag: u32, transfer: u32) -> Option<Status> {
    if bytes.len() != CSW_SIZE {
        return None;
    }
    let u32_at =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    if u32_at(0) != CSW_SIGNATURE || u32_at(4) != tag {
        return None;
    }
    let residue = u32_at(8);
    match bytes[12] {
        0 if residue <= transfer => Some(Status::Passed { residue }),
        1 if residue <= transfer => Some(Status::Failed { residue }),
        2 => Some(Status::PhaseError),
        _ => None,
    }
}

/// Bulk-Only Mass Storage Reset (class request to the interface).
pub fn reset(interface: u8) -> Setup {
    Setup {
        request_type: 0x21,
        request: 0xff,
        value: 0,
        index: u16::from(interface),
        length: 0,
    }
}

/// Get Max LUN: one byte, the highest LUN (devices may stall it: 0).
pub fn get_max_lun(interface: u8) -> Setup {
    Setup {
        request_type: 0xa1,
        request: 0xfe,
        value: 0,
        index: u16::from(interface),
        length: 1,
    }
}

/// The parts of an INQUIRY reply worth showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inquiry {
    /// 0: direct-access block device.
    pub device_type: u8,
    pub removable: bool,
    pub vendor: [u8; 8],
    pub product: [u8; 16],
}

impl Inquiry {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..36)?;
        let mut vendor = [0u8; 8];
        let mut product = [0u8; 16];
        for (out, &byte) in vendor
            .iter_mut()
            .zip(&bytes[8..16])
            .chain(product.iter_mut().zip(&bytes[16..32]))
        {
            *out = if (0x20..0x7f).contains(&byte) {
                byte
            } else {
                b' '
            };
        }
        Some(Self {
            device_type: bytes[0] & 0x1f,
            removable: bytes[1] & 0x80 != 0,
            vendor,
            product,
        })
    }

    /// Vendor and product, trimmed.
    pub fn vendor(&self) -> &str {
        core::str::from_utf8(&self.vendor).unwrap_or("").trim()
    }

    pub fn product(&self) -> &str {
        core::str::from_utf8(&self.product).unwrap_or("").trim()
    }
}

/// Blocks and block size from READ CAPACITY. `None` from (10) when the
/// device needs (16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capacity {
    pub blocks: u64,
    pub block_size: u32,
}

pub fn capacity_10(bytes: &[u8]) -> Option<Capacity> {
    let bytes = bytes.get(..8)?;
    let last = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let block_size = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    if last == u32::MAX || block_size == 0 {
        return None;
    }
    Some(Capacity {
        blocks: u64::from(last) + 1,
        block_size,
    })
}

pub fn capacity_16(bytes: &[u8]) -> Option<Capacity> {
    let bytes = bytes.get(..12)?;
    let last = u64::from_be_bytes(bytes[..8].try_into().ok()?);
    let block_size = u32::from_be_bytes(bytes[8..12].try_into().ok()?);
    if block_size == 0 || last == u64::MAX {
        return None;
    }
    Some(Capacity {
        blocks: last + 1,
        block_size,
    })
}

/// Sense key and additional sense code (fixed format, SPC-4 §4.5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sense {
    pub key: u8,
    pub code: u8,
    pub qualifier: u8,
}

pub const SENSE_NOT_READY: u8 = 0x2;
pub const SENSE_UNIT_ATTENTION: u8 = 0x6;
pub const SENSE_DATA_PROTECT: u8 = 0x7;
/// Additional sense code: medium not present.
pub const ASC_NO_MEDIUM: u8 = 0x3a;

impl Sense {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..14)?;
        if bytes[0] & 0x7e != 0x70 {
            return None;
        }
        Some(Self {
            key: bytes[2] & 0x0f,
            code: bytes[12],
            qualifier: bytes[13],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_commands() {
        let command = Command::read(0x1234, 8, 512);
        assert_eq!(command.transfer, 4096);
        assert_eq!(
            &command.bytes[..10],
            &[0x28, 0, 0, 0, 0x12, 0x34, 0, 0, 8, 0]
        );
        let wrapper = cbw(7, 0, &command);
        assert_eq!(&wrapper[..4], b"USBC");
        assert_eq!(&wrapper[4..8], &7u32.to_le_bytes());
        assert_eq!(&wrapper[8..12], &4096u32.to_le_bytes());
        assert_eq!((wrapper[12], wrapper[13], wrapper[14]), (0x80, 0, 10));
        assert_eq!(wrapper[15], 0x28);
        let write = Command::write(1, 1, 512);
        assert_eq!(cbw(1, 0, &write)[12], 0);
        assert_eq!(write.direction, Direction::Out);
    }

    #[test]
    fn large_addresses_use_sixteen_byte_commands() {
        let command = Command::read(1 << 33, 1, 512);
        assert_eq!(command.len, 16);
        assert_eq!(command.bytes[0], 0x88);
        assert_eq!(&command.bytes[2..10], &(1u64 << 33).to_be_bytes());
        assert_eq!(&command.bytes[10..14], &1u32.to_be_bytes());
        assert_eq!(Command::write(0, 70_000, 512).bytes[0], 0x8a);
    }

    #[test]
    fn checks_status_wrappers() {
        let mut csw_bytes = [0u8; CSW_SIZE];
        csw_bytes[..4].copy_from_slice(b"USBS");
        csw_bytes[4..8].copy_from_slice(&9u32.to_le_bytes());
        csw_bytes[8..12].copy_from_slice(&512u32.to_le_bytes());
        assert_eq!(
            csw(&csw_bytes, 9, 4096),
            Some(Status::Passed { residue: 512 })
        );
        assert_eq!(csw(&csw_bytes, 8, 4096), None, "wrong tag");
        assert_eq!(csw(&csw_bytes, 9, 100), None, "residue beyond the transfer");
        csw_bytes[12] = 1;
        assert_eq!(
            csw(&csw_bytes, 9, 4096),
            Some(Status::Failed { residue: 512 })
        );
        csw_bytes[12] = 2;
        assert_eq!(csw(&csw_bytes, 9, 0), Some(Status::PhaseError));
        csw_bytes[12] = 3;
        assert_eq!(csw(&csw_bytes, 9, 4096), None);
        assert_eq!(csw(&csw_bytes[..12], 9, 4096), None);
        csw_bytes[0] = b'X';
        assert_eq!(csw(&csw_bytes, 9, 4096), None);
    }

    #[test]
    fn parses_replies() {
        let mut inquiry = [0u8; 36];
        inquiry[1] = 0x80;
        inquiry[8..16].copy_from_slice(b"QEMU    ");
        inquiry[16..32].copy_from_slice(b"QEMU HARDDISK\x01  ");
        let parsed = Inquiry::parse(&inquiry).unwrap();
        assert!(parsed.removable);
        assert_eq!(
            (parsed.vendor(), parsed.product()),
            ("QEMU", "QEMU HARDDISK")
        );
        assert!(Inquiry::parse(&inquiry[..35]).is_none());

        assert_eq!(
            capacity_10(&[0, 0, 0x1f, 0xff, 0, 0, 2, 0]),
            Some(Capacity {
                blocks: 0x2000,
                block_size: 512
            })
        );
        assert_eq!(capacity_10(&[0xff, 0xff, 0xff, 0xff, 0, 0, 2, 0]), None);
        let mut long = [0u8; 32];
        long[..8].copy_from_slice(&(5_000_000_000u64 - 1).to_be_bytes());
        long[8..12].copy_from_slice(&4096u32.to_be_bytes());
        assert_eq!(
            capacity_16(&long).map(|c| (c.blocks, c.block_size)),
            Some((5_000_000_000, 4096))
        );

        let mut sense = [0u8; 18];
        sense[0] = 0x70;
        sense[2] = SENSE_NOT_READY;
        sense[12] = ASC_NO_MEDIUM;
        assert_eq!(
            Sense::parse(&sense),
            Some(Sense {
                key: 2,
                code: 0x3a,
                qualifier: 0
            })
        );
        sense[0] = 0;
        assert_eq!(Sense::parse(&sense), None);
    }

    #[test]
    fn encodes_class_requests() {
        assert_eq!(reset(0).to_bytes(), [0x21, 0xff, 0, 0, 0, 0, 0, 0]);
        assert_eq!(get_max_lun(1).to_bytes(), [0xa1, 0xfe, 0, 0, 1, 0, 1, 0]);
    }
}
