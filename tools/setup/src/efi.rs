//! UEFI boot entries (UEFI 2.10, §3.1.3 load options, §10.3 device paths):
//! what Oceans Setup writes to the firmware's `Boot####`, `BootOrder` and
//! `BootNext` variables. Pure encoding, tested on any host.

/// The variables' vendor GUID (EFI_GLOBAL_VARIABLE).
pub const GLOBAL: &str = "{8BE4DF61-93CA-11D2-AA0D-00E098032B8C}";
/// `LOAD_OPTION_ACTIVE`.
const ACTIVE: u32 = 1;

/// A GPT partition, as a hard-drive device path node names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partition {
    /// From 1, as in the GPT.
    pub number: u32,
    pub first_lba: u64,
    pub sectors: u64,
    /// The partition's GUID, as stored on disk.
    pub guid: [u8; 16],
}

fn utf16z(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(core::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// The device path of `file` (`\EFI\...`) on `partition`: a hard-drive
/// node, a file-path node, the end.
pub fn device_path(partition: &Partition, file: &str) -> Vec<u8> {
    let mut path = Vec::new();
    // Media device path, hard drive (type 4, subtype 1), 42 bytes.
    path.extend_from_slice(&[4, 1]);
    path.extend_from_slice(&42u16.to_le_bytes());
    path.extend_from_slice(&partition.number.to_le_bytes());
    path.extend_from_slice(&partition.first_lba.to_le_bytes());
    path.extend_from_slice(&partition.sectors.to_le_bytes());
    path.extend_from_slice(&partition.guid);
    // Partition format GPT (2), signature type GUID (2).
    path.extend_from_slice(&[2, 2]);
    // Media device path, file path (type 4, subtype 4).
    let name = utf16z(file);
    path.extend_from_slice(&[4, 4]);
    path.extend_from_slice(&(4 + name.len() as u16).to_le_bytes());
    path.extend_from_slice(&name);
    // End of the whole path.
    path.extend_from_slice(&[0x7f, 0xff, 4, 0]);
    path
}

/// An `EFI_LOAD_OPTION`: active, named `description`, booting `path`.
pub fn load_option(description: &str, path: &[u8]) -> Vec<u8> {
    let mut option = Vec::new();
    option.extend_from_slice(&ACTIVE.to_le_bytes());
    option.extend_from_slice(&(path.len() as u16).to_le_bytes());
    option.extend_from_slice(&utf16z(description));
    option.extend_from_slice(path);
    option
}

/// `BootOrder`'s entries.
pub fn boot_order(bytes: &[u8]) -> Vec<u16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .collect()
}

pub fn encode_order(order: &[u16]) -> Vec<u8> {
    order.iter().flat_map(|n| n.to_le_bytes()).collect()
}

/// The variable's name for entry `number`.
pub fn entry_name(number: u16) -> String {
    format!("Boot{number:04X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARTITION: Partition = Partition {
        number: 1,
        first_lba: 2048,
        sectors: 1_048_576,
        guid: [0xab; 16],
    };

    #[test]
    fn a_device_path_names_the_partition_and_the_file() {
        let path = device_path(&PARTITION, "\\EFI\\BOOT\\BOOTX64.EFI");
        assert_eq!(&path[..4], &[4, 1, 42, 0]);
        assert_eq!(&path[4..8], &1u32.to_le_bytes());
        assert_eq!(&path[8..16], &2048u64.to_le_bytes());
        assert_eq!(&path[16..24], &1_048_576u64.to_le_bytes());
        assert_eq!(&path[24..40], &[0xab; 16]);
        assert_eq!(&path[40..42], &[2, 2]);
        // The file node: 4 + the name in UTF-16 with its terminator.
        let name_len = ("\\EFI\\BOOT\\BOOTX64.EFI".len() + 1) * 2;
        assert_eq!(&path[42..44], &[4, 4]);
        assert_eq!(
            u16::from_le_bytes([path[44], path[45]]) as usize,
            4 + name_len
        );
        assert_eq!(&path[path.len() - 4..], &[0x7f, 0xff, 4, 0]);
        assert_eq!(path.len(), 42 + 4 + name_len + 4);
    }

    #[test]
    fn a_load_option_is_active_and_named() {
        let path = device_path(&PARTITION, "\\EFI\\BOOT\\BOOTX64.EFI");
        let option = load_option("Oceans", &path);
        assert_eq!(&option[..4], &1u32.to_le_bytes());
        assert_eq!(
            u16::from_le_bytes([option[4], option[5]]) as usize,
            path.len()
        );
        assert_eq!(&option[6..20], &utf16z("Oceans")[..]);
        assert_eq!(&option[20..], &path[..]);
    }

    #[test]
    fn boot_order_and_entry_names() {
        let order = boot_order(&[0x01, 0x00, 0x03, 0x00, 0x00, 0x00]);
        assert_eq!(order, [1, 3, 0]);
        assert_eq!(encode_order(&order), [1, 0, 3, 0, 0, 0]);
        assert_eq!(entry_name(0x2a), "Boot002A");
    }
}
