//! The Realtek RTL8111/8168 Gigabit Ethernet controllers (ADR-0123),
//! host-tested and without allocation: the most common Ethernet on PC
//! boards that are not Intel's, in a dozen silicon revisions under one PCI
//! ID. QEMU has no model of it (its `rtl8139` is an older, different chip),
//! so this logic is tested here and the driver on real machines.
//!
//! - [`Revision`]: told apart by `TxConfig`, with what differs between
//!   them ([`Revision::gated`] for the 8168G on, the station address in
//!   the ID registers from the 8168C on).
//! - [`reg`]: the registers, by offset and width.
//! - Descriptors: 16 bytes, the same layout both ways, owned by the device
//!   while `OWN` is set; no tail register, the device follows `OWN` round
//!   the ring.
//! - The PHY through `PHYAR` (one internal PHY, no address).
//!
//! The firmware-free path BSD's `re(4)` takes (FreeBSD's `if_re.c`,
//! OpenBSD's `re.c`, BSD licences), written here anew. No PHY or MAC
//! microcode is loaded.

#![no_std]

pub const VENDOR_REALTEK: u16 = 0x10ec;
pub const DESCRIPTOR_SIZE: usize = 16;
/// Rings are 256-byte aligned.
pub const RING_ALIGN: usize = 256;

/// The PCI IDs taken: the 8168 itself, its newer ID, the Killer E2600 and
/// E2500 (8168 cores), and TP-Link's TG-3468.
pub const DEVICES: &[(u16, u16)] = &[
    (0x10ec, 0x8168),
    (0x10ec, 0x8161),
    (0x10ec, 0x2600),
    (0x10ec, 0x2502),
    (0x10ff, 0x8168),
];

pub fn known(vendor: u16, device: u16) -> bool {
    DEVICES.contains(&(vendor, device))
}

/// The memory BAR with the registers (BAR 0 is I/O ports).
pub const REGISTER_BAR: u8 = 2;
pub const REGISTER_BAR_FALLBACK: u8 = 1;
/// The BAR must reach the last register used.
pub const SPAN: usize = reg::MISC + 4;

/// The silicon revision, from `TxConfig`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Revision {
    B,
    C,
    Cp,
    D,
    Dp,
    E,
    EVl,
    F,
    R8411,
    G,
    Ep,
    Gu,
    H,
    Fp,
    R8411B,
}

/// `TxConfig`'s hardware revision bits.
pub const HWREV_MASK: u32 = 0x7cc0_0000;

impl Revision {
    pub fn from_tx_config(tx_config: u32) -> Option<Self> {
        Some(match tx_config & HWREV_MASK {
            0x3000_0000 | 0x3800_0000 | 0x3840_0000 => Self::B,
            0x3c00_0000 | 0x3c40_0000 => Self::C,
            0x3c80_0000 => Self::Cp,
            0x2800_0000 => Self::D,
            0x2880_0000 => Self::Dp,
            0x2c00_0000 => Self::E,
            0x2c80_0000 => Self::EVl,
            0x4800_0000 => Self::F,
            0x4880_0000 => Self::R8411,
            0x4c00_0000 => Self::G,
            0x5000_0000 => Self::Ep,
            0x5080_0000 => Self::Gu,
            0x5400_0000 => Self::H,
            0x5480_0000 => Self::Fp,
            0x5c80_0000 => Self::R8411B,
            _ => return None,
        })
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::B => "RTL8168B",
            Self::C => "RTL8168C",
            Self::Cp => "RTL8168CP",
            Self::D => "RTL8168D",
            Self::Dp => "RTL8168DP",
            Self::E => "RTL8168E",
            Self::EVl => "RTL8168E-VL",
            Self::F => "RTL8168F",
            Self::R8411 => "RTL8411",
            Self::G => "RTL8168G",
            Self::Ep => "RTL8168EP",
            Self::Gu => "RTL8168GU",
            Self::H => "RTL8168H",
            Self::Fp => "RTL8168FP",
            Self::R8411B => "RTL8411B",
        }
    }

    /// The 8168G on: the receive path is gated (`MISC` bit 19) until the
    /// driver opens it, and stopping waits on the FIFOs, not `STOPREQ`.
    pub const fn gated(self) -> bool {
        matches!(
            self,
            Self::G | Self::Ep | Self::Gu | Self::H | Self::Fp | Self::R8411B
        )
    }

    /// The station address is in the ID registers after reset (8168C on);
    /// the 8168B keeps it only in its serial EEPROM.
    pub const fn address_in_registers(self) -> bool {
        !matches!(self, Self::B)
    }

    /// The PHY is woken after reset (8168B to D) with two PHY writes.
    pub const fn phy_wake(self) -> bool {
        matches!(self, Self::B | Self::C | Self::Cp | Self::D)
    }

    /// `PMCH` bit 7 wakes the PHY (8168D and E).
    pub const fn phy_wake_pm(self) -> bool {
        matches!(self, Self::D | Self::E)
    }

    /// The multicast filter is broken (8168F): all multicast accepted, as
    /// the driver does anyway.
    pub const fn early_off(self) -> u32 {
        match self {
            Self::EVl | Self::F => 0x3800,
            _ if self.gated() => 0x0800,
            _ => 0,
        }
    }
}

/// Registers (MMIO offsets), with their widths in the names' comments.
pub mod reg {
    /// The station address, 6 bytes; written as two dwords, unlocked.
    pub const IDR0: usize = 0x00;
    pub const IDR4: usize = 0x04;
    /// The multicast hash, two dwords.
    pub const MAR0: usize = 0x08;
    pub const MAR4: usize = 0x0c;
    pub const TX_DESC_LOW: usize = 0x20;
    pub const TX_DESC_HIGH: usize = 0x24;
    /// 8-bit.
    pub const CHIP_CMD: usize = 0x37;
    /// 8-bit.
    pub const TX_POLL: usize = 0x38;
    /// 16-bit.
    pub const INTR_MASK: usize = 0x3c;
    /// 16-bit, write-1-to-clear.
    pub const INTR_STATUS: usize = 0x3e;
    pub const TX_CONFIG: usize = 0x40;
    pub const RX_CONFIG: usize = 0x44;
    pub const MISSED: usize = 0x4c;
    /// 8-bit.
    pub const CFG9346: usize = 0x50;
    /// 8-bit.
    pub const CONFIG1: usize = 0x52;
    pub const TIMER_INT: usize = 0x58;
    pub const PHYAR: usize = 0x60;
    /// 8-bit.
    pub const PHY_STATUS: usize = 0x6c;
    /// 8-bit.
    pub const PMCH: usize = 0x6f;
    /// 16-bit.
    pub const RX_MAX_SIZE: usize = 0xda;
    /// 16-bit.
    pub const CPLUS_CMD: usize = 0xe0;
    /// 16-bit.
    pub const INTR_MITIGATE: usize = 0xe2;
    pub const RX_DESC_LOW: usize = 0xe4;
    pub const RX_DESC_HIGH: usize = 0xe8;
    /// 8-bit.
    pub const MAX_TX_PACKET: usize = 0xec;
    pub const MISC: usize = 0xf0;
    /// 8-bit, the 8168G on.
    pub const MCU_CMD: usize = 0xd3;

    pub mod chip_cmd {
        pub const RESET: u8 = 0x10;
        pub const RX_ENABLE: u8 = 0x08;
        pub const TX_ENABLE: u8 = 0x04;
        pub const STOP_REQUEST: u8 = 0x80;
    }

    /// `TX_POLL`: the normal-priority queue has frames.
    pub const TX_POLL_NORMAL: u8 = 0x40;

    pub mod cfg9346 {
        pub const UNLOCK: u8 = 0xc0;
        pub const LOCK: u8 = 0x00;
    }

    /// `CONFIG1`: the driver has the device.
    pub const CONFIG1_DRIVER_LOADED: u8 = 0x20;
    /// `PMCH`: PHY wake.
    pub const PMCH_PHY_WAKE: u8 = 0x80;
    /// `MISC`: the receive gate (8168G on).
    pub const MISC_RXDV_GATE: u32 = 1 << 19;
    /// `TX_CONFIG`: the transmit queue is empty (8168E-VL on).
    pub const TX_QUEUE_EMPTY: u32 = 1 << 11;
    /// `MCU_CMD`: both FIFOs empty.
    pub const MCU_FIFOS_EMPTY: u8 = 0x30;

    /// Interrupt causes (16-bit).
    pub mod int {
        pub const RX_OK: u16 = 0x0001;
        pub const RX_ERROR: u16 = 0x0002;
        pub const TX_OK: u16 = 0x0004;
        pub const TX_ERROR: u16 = 0x0008;
        pub const RX_UNAVAILABLE: u16 = 0x0010;
        pub const LINK_CHANGE: u16 = 0x0020;
        pub const RX_FIFO_OVERFLOW: u16 = 0x0040;
        pub const TX_UNAVAILABLE: u16 = 0x0080;
        pub const SYSTEM_ERROR: u16 = 0x8000;
        pub const ALL: u16 = 0xffff;
    }

    /// `PHY_STATUS`.
    pub mod phy_status {
        pub const FULL_DUPLEX: u8 = 0x01;
        pub const LINK: u8 = 0x02;
        pub const MBPS_10: u8 = 0x04;
        pub const MBPS_100: u8 = 0x08;
        pub const MBPS_1000: u8 = 0x10;
    }
}

/// `CPlusCmd`, written before anything else: memory read multiple, the
/// MAC's statistics off, transmit on (as Realtek's 8168s want).
pub const CPLUS: u16 = 0x0089;
/// `TxConfig`: the standard inter-frame gap, unlimited DMA bursts.
pub const TX_CONFIG: u32 = 0x0300_0700;
/// `RxConfig`: no FIFO threshold, unlimited DMA bursts, the station
/// address, broadcasts and all multicast (IPv6, ADR-0043), plus the
/// revision's early-off bits.
pub fn rx_config(revision: Revision) -> u32 {
    const MATCH_STATION: u32 = 0x02;
    const MULTICAST: u32 = 0x04;
    const BROADCAST: u32 = 0x08;
    0xff00 | MATCH_STATION | MULTICAST | BROADCAST | revision.early_off()
}
/// `MaxTxPacketSize`, as both BSD drivers set it.
pub const MAX_TX_PACKET: u8 = 16;

/// The interrupt causes the driver handles.
pub const INTERRUPTS: u16 = reg::int::RX_OK
    | reg::int::RX_ERROR
    | reg::int::TX_OK
    | reg::int::TX_ERROR
    | reg::int::RX_UNAVAILABLE
    | reg::int::LINK_CHANGE
    | reg::int::RX_FIFO_OVERFLOW
    | reg::int::TX_UNAVAILABLE
    | reg::int::SYSTEM_ERROR;

/// The station address from `IDR0`/`IDR4`, if it is a usable one.
pub fn station(low: u32, high: u32) -> Option<[u8; 6]> {
    let [a, b, c, d] = low.to_le_bytes();
    let [e, f, _, _] = high.to_le_bytes();
    let mac = [a, b, c, d, e, f];
    let usable = mac != [0; 6] && mac != [0xff; 6] && a & 1 == 0;
    usable.then_some(mac)
}

/// `IDR0`/`IDR4` for `mac`.
pub fn station_registers(mac: &[u8; 6]) -> (u32, u32) {
    (
        u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]),
        u32::from_le_bytes([mac[4], mac[5], 0, 0]),
    )
}

/// Descriptor flags, in the first dword.
pub mod desc {
    pub const OWN: u32 = 1 << 31;
    pub const END_OF_RING: u32 = 1 << 30;
    pub const FIRST: u32 = 1 << 29;
    pub const LAST: u32 = 1 << 28;
    pub const LENGTH: u32 = 0x3fff;
    /// Receive: any error (the 8168's position, one above the 8139's).
    pub const RX_ERROR: u32 = 1 << 21;
    /// Transmit: any error.
    pub const TX_ERROR: u32 = 1 << 23;
}

/// A receive descriptor as posted: `buffer` bytes at `address`, owned by
/// the device; `last` marks the end of the ring.
pub fn rx_posted(address: u64, buffer: usize, last: bool) -> [u64; 2] {
    let mut command = desc::OWN | (buffer as u32 & 0x1fff);
    if last {
        command |= desc::END_OF_RING;
    }
    [u64::from(command), address]
}

/// A transmit descriptor for one whole frame, handed to the device.
pub fn tx_frame(address: u64, len: u16, last: bool) -> [u64; 2] {
    let mut command = desc::OWN | desc::FIRST | desc::LAST | u32::from(len);
    if last {
        command |= desc::END_OF_RING;
    }
    [u64::from(command), address]
}

/// An empty transmit descriptor, the driver's; `last` ends the ring.
pub fn tx_empty(last: bool) -> [u64; 2] {
    [
        if last {
            u64::from(desc::END_OF_RING)
        } else {
            0
        },
        0,
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxOutcome {
    /// The device still owns it.
    Pending,
    /// A whole frame of this many bytes (the CRC taken off).
    Frame(usize),
    /// An error, a frame over several buffers, or a length that cannot be.
    Bad,
}

/// What a receive descriptor's first dword says (the device is not
/// trusted to write back sane values).
pub fn rx_outcome(status: u32, buffer: usize) -> RxOutcome {
    if status & desc::OWN != 0 {
        return RxOutcome::Pending;
    }
    let whole = status & (desc::FIRST | desc::LAST) == desc::FIRST | desc::LAST;
    let len = (status & desc::LENGTH) as usize;
    if status & desc::RX_ERROR != 0 || !whole || len < 14 + 4 || len > buffer.min(1514 + 4) {
        return RxOutcome::Bad;
    }
    RxOutcome::Frame(len - 4)
}

/// The PHY through `PHYAR`: register in bits 20:16, data in 15:0, bit 31
/// the flag (a write sets it and waits for it to clear; a read clears it
/// and waits for it to be set).
pub mod phyar {
    pub const FLAG: u32 = 1 << 31;

    pub fn read(register: u8) -> u32 {
        u32::from(register & 0x1f) << 16
    }

    pub fn write(register: u8, value: u16) -> u32 {
        FLAG | (u32::from(register & 0x1f) << 16) | u32::from(value)
    }

    /// Clause-22 registers used.
    pub const CONTROL: u8 = 0;
    pub const ADVERTISE: u8 = 4;
    pub const GIGABIT_CONTROL: u8 = 9;
    /// The page select of Realtek's PHYs, and the register the PHY wake
    /// clears.
    pub const PAGE_SELECT: u8 = 0x1f;
    pub const WAKE: u8 = 0x0e;
    pub const RESET: u16 = 0x8000;
    pub const AUTONEG: u16 = 0x1000 | 0x0200;
    /// 10 and 100 Mb/s, half and full duplex, symmetric pause, IEEE 802.3.
    pub const ADVERTISE_ALL: u16 = 0x01e1 | 0x0400;
    /// 1000 Mb/s full and half duplex.
    pub const ADVERTISE_GIGABIT: u16 = 0x0300;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Down,
    Up { mbps: u16, full_duplex: bool },
}

impl Link {
    pub fn from_phy_status(status: u8) -> Self {
        use reg::phy_status::*;
        if status & LINK == 0 {
            return Self::Down;
        }
        let mbps = if status & MBPS_1000 != 0 {
            1000
        } else if status & MBPS_100 != 0 {
            100
        } else {
            10
        };
        Self::Up {
            mbps,
            full_duplex: status & FULL_DUPLEX != 0,
        }
    }
}

impl core::fmt::Display for Link {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Down => f.write_str("down"),
            Self::Up { mbps, full_duplex } => write!(
                f,
                "up, {mbps} Mb/s {} duplex",
                if full_duplex { "full" } else { "half" }
            ),
        }
    }
}

#[cfg(test)]
mod tests;
