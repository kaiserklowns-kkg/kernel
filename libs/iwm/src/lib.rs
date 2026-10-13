//! The Intel Wireless 8260 ("8000 family", ADR-0129), host-tested: what
//! the `iwm` driver computes and checks, the device left to it.
//!
//! - [`fw`]: the firmware file.
//! - [`reg`]: registers (CSR in BAR 0; PRPH and SRAM reached through it).
//! - [`cmd`]: host command headers, TFDs, scheduler byte counts.
//! - [`rx`]: packets in an RX buffer: responses and notifications.
//! - [`alive`], [`nvm`], [`paging`]: what the bring-up reads and sends.
//! - [`mvm`]: the commands after ALIVE, scanning, received frames.
//! - [`join`]: joining a network, and sending frames.
//!
//! Facts from OpenBSD's `iwm(4)` (ISC licence), written here anew.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod fw;
pub mod join;
pub mod mvm;

pub const VENDOR_INTEL: u16 = 0x8086;
/// The 8260's PCI IDs.
pub const DEVICES: &[u16] = &[0x24f3, 0x24f4];

/// Registers (BAR 0 offsets, PRPH addresses).
pub mod reg {
    pub const HW_IF_CONFIG: usize = 0x000;
    pub const INT_COALESCING: usize = 0x004;
    pub const INT_PERIODIC: usize = 0x005;
    pub const INT: usize = 0x008;
    pub const INT_MASK: usize = 0x00c;
    pub const FH_INT_STATUS: usize = 0x010;
    pub const RESET: usize = 0x020;
    pub const GP_CNTRL: usize = 0x024;
    pub const HW_REV: usize = 0x028;
    pub const GIO: usize = 0x03c;
    pub const UCODE_DRV_GP1_CLR: usize = 0x05c;
    pub const MBOX_SET: usize = 0x088;
    pub const MAC_SHADOW_REG_CTRL: usize = 0x0a8;
    pub const GIO_CHICKEN_BITS: usize = 0x100;
    pub const DBG_HPET_MEM: usize = 0x240;
    pub const DBG_LINK_PWR_MGMT: usize = 0x250;
    pub const HBUS_TARG_MEM_RADDR: usize = 0x40c;
    pub const HBUS_TARG_MEM_WADDR: usize = 0x410;
    pub const HBUS_TARG_MEM_WDAT: usize = 0x418;
    pub const HBUS_TARG_MEM_RDAT: usize = 0x41c;
    pub const HBUS_TARG_PRPH_WADDR: usize = 0x444;
    pub const HBUS_TARG_PRPH_RADDR: usize = 0x448;
    pub const HBUS_TARG_PRPH_WDAT: usize = 0x44c;
    pub const HBUS_TARG_PRPH_RDAT: usize = 0x450;
    pub const HBUS_TARG_WRPTR: usize = 0x460;
    /// Firmware load progress (8000 family).
    pub const FH_UCODE_LOAD_STATUS: usize = 0x1af0;
    pub const FH_KW_MEM_ADDR: usize = 0x197c;
    pub const FH_TX_CHICKEN_BITS: usize = 0x1e98;
    /// RX channel 0.
    pub const RSCSR_STTS_WPTR: usize = 0x1bc0;
    pub const RSCSR_RBDCB_BASE: usize = 0x1bc4;
    pub const RSCSR_WPTR: usize = 0x1bc8;
    pub const RSCSR_RDPTR: usize = 0x1bcc;
    pub const RCSR_CONFIG: usize = 0x1c00;
    pub const RCSR_RBDCB_WPTR: usize = 0x1c08;
    pub const RCSR_FLUSH_RB_REQ: usize = 0x1c10;
    pub const RSSR_STATUS: usize = 0x1c44;
    /// The value that turns RX on: 256 buffers of 4 KiB, interrupts to the
    /// host.
    pub const RCSR_ENABLE: u32 = 0x8080_1114;
    pub const SPAN: usize = 0x2000;

    /// TX channel `ch`'s registers, and the service channel (9).
    pub const fn tcsr_config(ch: usize) -> usize {
        0x1d00 + 0x20 * ch
    }
    pub const fn tcsr_buf_sts(ch: usize) -> usize {
        0x1d00 + 0x20 * ch + 8
    }
    pub const fn tfdib_ctrl0(ch: usize) -> usize {
        0x1900 + 8 * ch
    }
    pub const fn tfdib_ctrl1(ch: usize) -> usize {
        0x1900 + 8 * ch + 4
    }
    pub const SRVC_CHANNEL: usize = 9;
    pub const SRVC_SRAM_ADDR: usize = 0x19c8;
    /// TX queue `q`'s ring base (`paddr >> 8`).
    pub const fn cbbc_queue(q: usize) -> usize {
        match q {
            0..16 => 0x19d0 + 4 * q,
            16..20 => 0x1bf0 + 4 * (q - 16),
            _ => 0x1b20 + 4 * (q - 20),
        }
    }

    pub mod hw_if {
        pub const HAP_WAKE_L1A: u32 = 1 << 19;
        pub const NIC_READY: u32 = 1 << 22;
        pub const PREPARE: u32 = 1 << 27;
        pub const ENABLE_PME: u32 = 1 << 28;
        /// The fields `nic_config` rewrites.
        pub const MAC_DASH: u32 = 0b11;
        pub const MAC_STEP: u32 = 0b11 << 2;
        pub const MAC_SI: u32 = 1 << 8;
        pub const RADIO_SI: u32 = 1 << 9;
        pub const PHY_TYPE: u32 = 0b11 << 10;
        pub const PHY_DASH: u32 = 0b11 << 12;
        pub const PHY_STEP: u32 = 0b11 << 14;
    }

    pub mod gp {
        pub const MAC_CLOCK_READY: u32 = 1 << 0;
        pub const INIT_DONE: u32 = 1 << 2;
        pub const MAC_ACCESS_REQ: u32 = 1 << 3;
        pub const GOING_TO_SLEEP: u32 = 1 << 4;
        /// Clear: the radio is switched off (RF-kill).
        pub const HW_RF_KILL_SW: u32 = 1 << 27;
    }

    pub mod int {
        pub const ALIVE: u32 = 1 << 0;
        pub const WAKEUP: u32 = 1 << 1;
        pub const SW_RX: u32 = 1 << 3;
        pub const CT_KILL: u32 = 1 << 6;
        pub const RF_KILL: u32 = 1 << 7;
        pub const SW_ERR: u32 = 1 << 25;
        pub const FH_TX: u32 = 1 << 27;
        pub const RX_PERIODIC: u32 = 1 << 28;
        pub const HW_ERR: u32 = 1 << 29;
        pub const FH_RX: u32 = 1 << 31;
        pub const INI_SET_MASK: u32 =
            FH_RX | HW_ERR | FH_TX | SW_ERR | RF_KILL | SW_RX | WAKEUP | ALIVE | RX_PERIODIC;
        pub const FH_STATUS_TX: u32 = 0x3;
        pub const FH_STATUS_RX: u32 = 0x4003_0000;
    }

    pub const RESET_SW: u32 = 0x80;
    pub const RESET_MASTER_DISABLED: u32 = 0x100;
    pub const RESET_STOP_MASTER: u32 = 0x200;
    pub const GP1_SW_BIT_RFKILL: u32 = 1 << 1;
    pub const GP1_CMD_BLOCKED: u32 = 1 << 2;
    pub const MBOX_OS_ALIVE: u32 = 0x20;
    pub const GIO_L0S_ENABLED: u32 = 1 << 1;
    pub const GIO_CHICKEN_L1A_NO_L0S_RX: u32 = 0x0080_0000;

    /// Periphery registers.
    pub mod prph {
        pub const RELEASE_CPU_RESET: u32 = 0x300c;
        pub const RELEASE_CPU_RESET_BIT: u32 = 0x0100_0000;
        pub const WFPM_CTRL: u32 = 0xa0_3030;
        pub const ENABLE_WFPM: u32 = 0x8000_0000;
        pub const AUX_MISC: u32 = 0xa2_00b0;
        pub const WFMP_MAC_ADDR_0: u32 = 0xa0_3080;
        pub const WFMP_MAC_ADDR_1: u32 = 0xa0_3084;
        pub const SCD_SRAM_BASE: u32 = 0xa0_2c00;
        pub const SCD_DRAM_BASE: u32 = 0xa0_2c08;
        pub const SCD_TXFACT: u32 = 0xa0_2c10;
        pub const SCD_GP_CTRL: u32 = 0xa0_2da8;
        pub const SCD_CHAINEXT_EN: u32 = 0xa0_2e44;
        pub const SCD_AGGR_SEL: u32 = 0xa0_2e48;
        pub const SCD_EN_CTRL: u32 = 0xa0_2e54;
        pub const fn scd_queue_rdptr(q: u32) -> u32 {
            0xa0_2c68 + 4 * q
        }
        pub const fn scd_queue_status(q: u32) -> u32 {
            0xa0_2d0c + 4 * q
        }
        pub const SCD_GP_AUTO_ACTIVE: u32 = 1 << 18;
        pub const SCD_GP_ENABLE_31_QUEUES: u32 = 1 << 0;
    }
}

/// Host commands (§5.4 of the spec): header, TFD, byte counts.
pub mod cmd {
    /// Command IDs used (group 0 unless said).
    pub const NVM_ACCESS: u8 = 0x88;
    pub const TX_ANT_CONFIGURATION: u8 = 0x98;
    pub const PHY_CONFIGURATION: u8 = 0x6a;
    pub const PHY_DB: u8 = 0x6c;
    pub const BT_CONFIG: u8 = 0x9b;
    pub const SF_CFG: u8 = 0xd1;
    /// Group 1 (long).
    pub const FW_PAGING_BLOCK: u8 = 0x4f;
    pub const LONG_GROUP: u8 = 1;
    /// Group 5 (data path).
    pub const DQA_ENABLE: u8 = 0x00;
    pub const DATA_PATH_GROUP: u8 = 5;

    /// Notifications.
    pub const ALIVE: u8 = 0x01;
    pub const REPLY_ERROR: u8 = 0x02;
    pub const INIT_COMPLETE: u8 = 0x04;
    pub const CALIB_RES_PHY_DB: u8 = 0x6b;

    pub const TFD_SIZE: usize = 128;
    pub const RING: usize = 256;
    /// Payload room per command slot (legacy header).
    pub const SLOT: usize = 512;
    /// The largest command: header and payload, in one 4 KiB buffer.
    pub const MAX: usize = 4096;
    /// TX queues the scheduler knows, and each one's byte-count table
    /// (256 entries and the first 64 again), in bytes.
    pub const QUEUES: usize = 31;
    pub const BC_TABLE: usize = 2 * (RING + 64);

    /// The header of a command to queue `qid`, slot `idx`: legacy (4 bytes)
    /// for group 0, wide (8 bytes) otherwise. Returns its length.
    pub fn header(
        out: &mut [u8; 8],
        opcode: u8,
        group: u8,
        version: u8,
        idx: u8,
        qid: u8,
        payload: u16,
    ) -> usize {
        if group == 0 {
            *out = [opcode, 0, idx, qid, 0, 0, 0, 0];
            4
        } else {
            let len = payload.to_le_bytes();
            *out = [opcode, group, idx, qid, len[0], len[1], 0, version];
            8
        }
    }

    /// A TFD with one buffer: `len` bytes at `paddr`.
    pub fn tfd(paddr: u64, len: u16) -> [u8; TFD_SIZE] {
        let mut tfd = [0u8; TFD_SIZE];
        tfd[3] = 1;
        tfd[4..8].copy_from_slice(&(paddr as u32).to_le_bytes());
        let hi = (((paddr >> 32) & 0xf) as u16) | (len << 4);
        tfd[8..10].copy_from_slice(&hi.to_le_bytes());
        tfd
    }

    /// The scheduler's byte-count entry for a command (in dwords when the
    /// firmware says so).
    pub fn byte_count(len: u16, dwords: bool) -> u16 {
        let len = len + 8;
        if dwords { len.div_ceil(4) } else { len }
    }
}

/// Packets in an RX buffer.
pub mod rx {
    /// One packet: the response to a host command, or a notification.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Packet<'a> {
        pub code: u8,
        /// For wide commands, the group.
        pub flags: u8,
        pub idx: u8,
        pub qid: u8,
        pub payload: &'a [u8],
    }

    impl Packet<'_> {
        /// Unsolicited, not an answer.
        pub fn notification(&self) -> bool {
            self.qid & 0x80 != 0
        }

        pub fn failed(&self) -> bool {
            self.flags & 0x40 != 0
        }
    }

    const FRAME_INVALID: u32 = 0x5555_0000;

    /// The packets of a 4 KiB RX buffer, each at a 64-byte boundary.
    pub fn packets(buffer: &[u8]) -> impl Iterator<Item = Packet<'_>> {
        let mut at = 0;
        core::iter::from_fn(move || {
            let word = u32::from_le_bytes(buffer.get(at..at + 4)?.try_into().ok()?);
            let header = buffer.get(at + 4..at + 8)?;
            let size = (word & 0x3fff) as usize;
            if word == FRAME_INVALID
                || (header[0] == 0 && header[2] == 0 && header[3] == 0)
                || size < 4
            {
                return None;
            }
            let payload = buffer.get(at + 8..at + 4 + size)?;
            let packet = Packet {
                code: header[0],
                flags: header[1],
                idx: header[2],
                qid: header[3],
                payload,
            };
            at = (at + 4 + size).next_multiple_of(64);
            Some(packet)
        })
    }
}

/// The ALIVE notification.
pub mod alive {
    pub const OK: u16 = 0xcafe;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Alive {
        pub status: u16,
        pub error_table: u32,
        pub scd_base: u32,
    }

    /// Its versions differ by length: 44, 64 or 68 bytes.
    pub fn parse(payload: &[u8]) -> Option<Alive> {
        let u32_at = |i: usize| -> Option<u32> {
            Some(u32::from_le_bytes(payload.get(i..i + 4)?.try_into().ok()?))
        };
        let status = u16::from_le_bytes(payload.get(..2)?.try_into().ok()?);
        // v3 (68) and v2 (64): status, flags, minor, major, 4 bytes of
        // ver/mac/opt, timestamp, then the pointers; v1 (44) is shorter.
        let (error_table, scd_base) = match payload.len() {
            68 | 64 => (u32_at(20)?, u32_at(40)?),
            44 => (u32_at(16)?, u32_at(36)?),
            _ => return None,
        };
        Some(Alive {
            status,
            error_table,
            scd_base,
        })
    }
}

/// NVM sections and the MAC address.
pub mod nvm {
    pub const SW: u16 = 1;
    pub const REGULATORY: u16 = 3;
    pub const HW_8000: u16 = 10;
    pub const MAC_OVERRIDE: u16 = 11;
    pub const PHY_SKU: u16 = 12;
    pub const CHUNK: u16 = 2048;

    /// The NVM_ACCESS read request for `section` at `offset`.
    pub fn read(section: u16, offset: u16) -> [u8; 8] {
        let s = section.to_le_bytes();
        let o = offset.to_le_bytes();
        let l = CHUNK.to_le_bytes();
        [0, 0, s[0], s[1], o[0], o[1], l[0], l[1]]
    }

    /// The answer: its data, if the read succeeded.
    pub fn answer(payload: &[u8]) -> Option<&[u8]> {
        let length = usize::from(u16::from_le_bytes(payload.get(2..4)?.try_into().ok()?));
        let status = u16::from_le_bytes(payload.get(6..8)?.try_into().ok()?);
        if status != 0 {
            return None;
        }
        payload.get(8..8 + length)
    }

    fn usable(mac: [u8; 6]) -> Option<[u8; 6]> {
        let reserved = [0x02, 0xcc, 0xaa, 0xff, 0xee, 0x00];
        (mac != reserved && mac != [0xff; 6] && mac != [0; 6] && mac[0] & 1 == 0).then_some(mac)
    }

    /// From the MAC_OVERRIDE section: 6 bytes at byte 2.
    pub fn mac_override(section: &[u8]) -> Option<[u8; 6]> {
        usable(section.get(2..8)?.try_into().ok()?)
    }

    /// From the WFMP registers (firmware running): a3 a2 a1 a0 b1 b0.
    pub fn mac_from_registers(a: u32, b: u32) -> Option<[u8; 6]> {
        let a = a.to_le_bytes();
        let b = b.to_le_bytes();
        usable([a[3], a[2], a[1], a[0], b[1], b[0]])
    }

    fn word(section: &[u8], index: usize) -> Option<u16> {
        Some(u16::from_le_bytes(
            section.get(2 * index..2 * index + 2)?.try_into().ok()?,
        ))
    }

    /// The NVM version (the software section's first word).
    pub fn version(sw: &[u8]) -> Option<u16> {
        word(sw, 0)
    }

    /// Whether the regulatory section turns location-aware regulatory
    /// (LAR) on: the firmware then decides the channels by country.
    pub fn lar_enabled(regulatory: &[u8], version: u16) -> bool {
        let offset = if version < 0xe39 { 0x4c7 } else { 0x507 };
        word(regulatory, offset).is_some_and(|w| w & 0x7 != 0)
    }

    /// What the PHY_SKU section says the radio has.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Sku {
        pub band_24ghz: bool,
        pub band_5ghz: bool,
        pub tx_ant: u8,
        pub rx_ant: u8,
    }

    pub fn sku(phy_sku: &[u8]) -> Option<Sku> {
        let radio = u32::from_le_bytes(phy_sku.get(..4)?.try_into().ok()?);
        let sku = u32::from_le_bytes(phy_sku.get(4..8)?.try_into().ok()?);
        Some(Sku {
            band_24ghz: sku & 1 != 0,
            band_5ghz: sku & 2 != 0,
            tx_ant: ((radio >> 24) & 0xf) as u8,
            rx_ant: ((radio >> 28) & 0xf) as u8,
        })
    }

    /// The 8000 family's channels, in the order of the regulatory
    /// section's flags.
    pub const CHANNELS: [u8; 51] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, // 2.4 GHz
        36, 40, 44, 48, 52, 56, 60, 64, 68, 72, 76, 80, 84, 88, 92, 96, 100, 104, 108, 112, 116,
        120, 124, 128, 132, 136, 140, 144, 149, 153, 157, 161, 165, 169, 173, 177, 181,
    ];
    const CHANNEL_VALID: u16 = 1 << 0;

    /// The channels the regulatory section allows (5 GHz only if the radio
    /// has it).
    pub fn channels(regulatory: &[u8], band_5ghz: bool) -> impl Iterator<Item = u8> + '_ {
        CHANNELS
            .iter()
            .enumerate()
            .filter_map(move |(i, &channel)| {
                let flags = word(regulatory, i)?;
                (flags & CHANNEL_VALID != 0 && (i < 14 || band_5ghz)).then_some(channel)
            })
    }
}

/// Paging (the REGULAR image's CPU2 code, held in host memory).
pub mod paging {
    pub const PAGE: usize = 4096;
    pub const BLOCK: usize = 32 * 1024;

    /// Pages, blocks (after the CSS block), and pages in the last.
    pub fn layout(size: usize) -> (usize, usize, usize) {
        let pages = size / PAGE;
        let blocks = (pages - 1) / 8 + 1;
        let last = pages - (blocks - 1) * 8;
        (pages, blocks, last)
    }

    /// `FW_PAGING_BLOCK_CMD`'s payload for the CSS block and `blocks`
    /// blocks at these physical addresses.
    pub fn command(addresses: &[u64], last_pages: usize) -> [u8; 144] {
        let mut out = [0u8; 144];
        let flags = (1u32 << 9) | (1 << 8) | last_pages as u32;
        out[..4].copy_from_slice(&flags.to_le_bytes());
        out[4..8].copy_from_slice(&15u32.to_le_bytes());
        out[8..12].copy_from_slice(&((addresses.len() - 1) as u32).to_le_bytes());
        for (i, address) in addresses.iter().enumerate().take(33) {
            out[12 + 4 * i..16 + 4 * i].copy_from_slice(&((address >> 12) as u32).to_le_bytes());
        }
        out
    }
}

#[cfg(test)]
mod join_tests;
#[cfg(test)]
mod tests;

/// The smart FIFO configuration for the INIT image: state INIT_OFF, the
/// default watermarks and timers (92 bytes).
pub fn sf_config_init() -> [u8; 92] {
    let mut out = [0u8; 92];
    let mut words = [0u32; 23];
    words[0] = 3;
    words[1] = 4096;
    words[2] = 8192;
    // Long delay: 1 s for every scenario's aging and idle timers.
    for word in &mut words[3..13] {
        *word = 1_000_000;
    }
    // Full on: aging 400, idle 160 (32 µs units) per scenario.
    for scenario in 0..5 {
        words[13 + 2 * scenario] = 400;
        words[14 + 2 * scenario] = 160;
    }
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    out
}
