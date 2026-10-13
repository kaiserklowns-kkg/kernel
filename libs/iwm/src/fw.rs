//! The firmware file (`iwlwifi-*.ucode`): an 88-byte header, then TLV
//! records; the images' sections, capabilities and defaults among them.
//! Unknown records are skipped (newer files carry some iwm never knew).

use alloc::vec::Vec;

const MAGIC: u32 = 0x0a4c_5749;
const HEADER: usize = 88;

/// The record types used.
mod tlv {
    pub const SEC_RT: u32 = 19;
    pub const SEC_INIT: u32 = 20;
    pub const DEF_CALIB: u32 = 22;
    pub const PHY_SKU: u32 = 23;
    pub const FLAGS: u32 = 18;
    pub const API_CHANGES_SET: u32 = 29;
    pub const ENABLED_CAPABILITIES: u32 = 30;
    pub const PAGING: u32 = 32;
    pub const N_SCAN_CHANNELS: u32 = 31;
    pub const FW_VERSION: u32 = 36;
}

/// Section load addresses that only mark where CPU2's sections and the
/// paging image start.
pub const CPU1_CPU2_SEPARATOR: u32 = 0xffff_cccc;
pub const PAGING_SEPARATOR: u32 = 0xaaaa_bbbb;

/// Image types, as `DEF_CALIB` names them.
pub const REGULAR: u32 = 0;
pub const INIT: u32 = 1;

/// One section: where it loads, and its bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section<'a> {
    pub devoff: u32,
    pub data: &'a [u8],
}

/// One image (INIT or REGULAR), split as the 8000 family loads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Image<'a> {
    pub cpu1: Vec<Section<'a>>,
    pub cpu2: Vec<Section<'a>>,
    /// The paging CSS and image (REGULAR only), loaded after ALIVE.
    pub paging_css: Option<&'a [u8]>,
    pub paging: Option<&'a [u8]>,
}

impl<'a> Image<'a> {
    fn from_sections(sections: &[Section<'a>]) -> Self {
        let mut image = Self::default();
        let mut part = 0;
        let mut after_paging = Vec::new();
        for section in sections {
            match section.devoff {
                CPU1_CPU2_SEPARATOR => part = 1,
                PAGING_SEPARATOR => part = 2,
                _ => match part {
                    0 => image.cpu1.push(*section),
                    1 => image.cpu2.push(*section),
                    _ => after_paging.push(section.data),
                },
            }
        }
        image.paging_css = after_paging.first().copied();
        image.paging = after_paging.get(1).copied();
        image
    }
}

/// Calibration triggers for an image (`PHY_CONFIGURATION_CMD`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Calibration {
    pub flow: u32,
    pub event: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Firmware<'a> {
    pub version: (u32, u32, u32),
    pub init: Image<'a>,
    pub regular: Image<'a>,
    pub phy_sku: u32,
    pub calib_init: Calibration,
    pub calib_regular: Calibration,
    pub capa_flags: u32,
    /// `ucode_api` and `enabled_capa` bits, 128 each.
    pub api: [u32; 4],
    pub capa: [u32; 4],
    pub paging_size: u32,
    pub n_scan_channels: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFirmware,
    Truncated,
    /// A record is malformed (its type).
    Bad(u32),
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

impl<'a> Firmware<'a> {
    pub fn parse(file: &'a [u8]) -> Result<Self, Error> {
        if u32_at(file, 0) != Some(0) || u32_at(file, 4) != Some(MAGIC) {
            return Err(Error::NotFirmware);
        }
        let mut fw = Self::default();
        let mut init = Vec::new();
        let mut regular = Vec::new();
        let mut at = HEADER;
        while at + 8 <= file.len() {
            let kind = u32_at(file, at).ok_or(Error::Truncated)?;
            let len = u32_at(file, at + 4).ok_or(Error::Truncated)? as usize;
            let data = file.get(at + 8..at + 8 + len).ok_or(Error::Truncated)?;
            let word = |i: usize| u32_at(data, 4 * i).ok_or(Error::Bad(kind));
            match kind {
                tlv::SEC_RT | tlv::SEC_INIT => {
                    let devoff = word(0)?;
                    let section = Section {
                        devoff,
                        data: &data[4..],
                    };
                    if kind == tlv::SEC_RT {
                        regular.push(section);
                    } else {
                        init.push(section);
                    }
                }
                tlv::DEF_CALIB => {
                    let calibration = Calibration {
                        flow: word(1)?,
                        event: word(2)?,
                    };
                    match word(0)? {
                        REGULAR => fw.calib_regular = calibration,
                        INIT => fw.calib_init = calibration,
                        _ => {}
                    }
                }
                tlv::PHY_SKU => fw.phy_sku = word(0)?,
                tlv::FLAGS => fw.capa_flags = word(0)?,
                tlv::API_CHANGES_SET | tlv::ENABLED_CAPABILITIES => {
                    let index = word(0)? as usize;
                    let bits = word(1)?;
                    let set = if kind == tlv::API_CHANGES_SET {
                        &mut fw.api
                    } else {
                        &mut fw.capa
                    };
                    if let Some(slot) = set.get_mut(index) {
                        *slot = bits;
                    }
                }
                tlv::PAGING => fw.paging_size = word(0)?,
                tlv::N_SCAN_CHANNELS => fw.n_scan_channels = word(0)?,
                tlv::FW_VERSION => fw.version = (word(0)?, word(1)?, word(2)?),
                // Everything else: not needed, or not known.
                _ => {}
            }
            at += 8 + len.next_multiple_of(4);
        }
        fw.init = Image::from_sections(&init);
        fw.regular = Image::from_sections(&regular);
        if fw.init.cpu1.is_empty() || fw.regular.cpu1.is_empty() {
            return Err(Error::Truncated);
        }
        Ok(fw)
    }

    /// Whether capability bit `n` is set.
    pub fn has_capa(&self, n: usize) -> bool {
        self.capa[n / 32] & (1 << (n % 32)) != 0
    }

    /// Scheduler byte counts in dwords (`FLAGS` bit 4).
    pub fn dword_byte_counts(&self) -> bool {
        self.capa_flags & (1 << 4) != 0
    }
}

/// Capability bits used.
pub mod capa {
    pub const LAR_SUPPORT: usize = 1;
    pub const UMAC_SCAN: usize = 2;
    pub const DQA_SUPPORT: usize = 12;
    pub const LAR_SUPPORT_V3: usize = 73;
    pub const CT_KILL_BY_FW: usize = 74;
}
