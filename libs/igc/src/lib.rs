//! Intel's 82576, I210/I211 and I225/I226 Ethernet controllers (ADR-0123),
//! host-tested and without allocation: the "igb" generation and the 2.5G
//! I225/I226 that grew out of it, on Intel's desktop and mini-PC boards
//! from 2019 on. QEMU's `-device igb` is an 82576, which is how the driver
//! is tested.
//!
//! Three families share the queue registers, the advanced descriptors and
//! the interrupt routing, and differ in a few places, all in [`Family`]:
//! the reset bit, where the interrupt cause registers are, the PHY's MDIO
//! address, and 2.5 Gb/s.
//!
//! Facts from Intel's open drivers (OpenBSD's `igc`, FreeBSD's `e1000`,
//! BSD and ISC licences) and QEMU's igb model, written here anew.

#![no_std]

pub use oceans_e1000e::{RxRing, TxRing, mac_from_receive_address, receive_address};

pub const VENDOR_INTEL: u16 = 0x8086;
/// QEMU's `-device igb`.
pub const DEVICE_82576: u16 = 0x10c9;
pub const DESCRIPTOR_SIZE: usize = 16;

/// What differs between the families.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// 82576 (and QEMU's igb).
    I82576,
    /// I210, I211.
    I210,
    /// I225, I226 (and the I220-V, I221-V): 2.5 Gb/s.
    I225,
}

/// Every device ID the driver takes: copper parts only (fibre and SerDes
/// need a PCS path this driver does not have).
pub const DEVICES: &[(u16, Family)] = &[
    (0x10c9, Family::I82576),
    (0x10e8, Family::I82576),
    (0x1526, Family::I82576),
    (0x150a, Family::I82576),
    (0x1533, Family::I210),
    (0x1534, Family::I210),
    (0x1535, Family::I210),
    (0x157b, Family::I210),
    (0x1539, Family::I210),
    (0x15f2, Family::I225),
    (0x15f3, Family::I225),
    (0x15f8, Family::I225),
    (0x3100, Family::I225),
    (0x3101, Family::I225),
    (0x5502, Family::I225),
    (0x0d9f, Family::I225),
    (0x15fd, Family::I225),
    (0x15f7, Family::I225),
    (0x125b, Family::I225),
    (0x125c, Family::I225),
    (0x125d, Family::I225),
    (0x3102, Family::I225),
    (0x5503, Family::I225),
    (0x125f, Family::I225),
    (0x125e, Family::I225),
];

pub fn family(device: u16) -> Option<Family> {
    DEVICES
        .iter()
        .find(|&&(id, _)| id == device)
        .map(|&(_, family)| family)
}

/// Whether the part has no NVM of its own: no address in receive
/// address 0 after reset.
pub fn blank_nvm(device: u16) -> bool {
    matches!(device, 0x15fd | 0x125f | 0x157b)
}

impl Family {
    /// `CTRL`'s reset bit: `RST`, or `DEV_RST` on the I225.
    pub const fn reset_bit(self) -> u32 {
        match self {
            Self::I225 => reg::ctrl::DEVICE_RESET,
            _ => reg::ctrl::RESET,
        }
    }

    /// The interrupt cause registers `ICR`, `IMS`, `IMC`: moved on the I225.
    pub const fn icr(self) -> usize {
        match self {
            Self::I225 => 0x1500,
            _ => 0x00c0,
        }
    }

    pub const fn ims(self) -> usize {
        match self {
            Self::I225 => 0x1508,
            _ => 0x00d0,
        }
    }

    pub const fn imc(self) -> usize {
        match self {
            Self::I225 => 0x150c,
            _ => 0x00d8,
        }
    }

    /// The internal PHY's MDIO address.
    pub const fn phy_address(self) -> u32 {
        match self {
            Self::I225 => 0,
            _ => 1,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::I82576 => "82576",
            Self::I210 => "I210/I211",
            Self::I225 => "I225/I226",
        }
    }
}

/// The registers used (one queue).
pub mod reg {
    pub const CTRL: usize = 0x0000;
    pub const STATUS: usize = 0x0008;
    pub const EECD: usize = 0x0010;
    pub const CTRL_EXT: usize = 0x0018;
    pub const MDIC: usize = 0x0020;
    pub const RCTL: usize = 0x0100;
    pub const TCTL: usize = 0x0400;
    pub const IPCNFG: usize = 0x0e38;
    pub const EEER: usize = 0x0e30;
    pub const PHPM: usize = 0x0e14;
    pub const GPIE: usize = 0x1514;
    pub const EIMS: usize = 0x1524;
    pub const EIMC: usize = 0x1528;
    pub const EIAC: usize = 0x152c;
    pub const EIAM: usize = 0x1530;
    pub const EICR: usize = 0x1580;
    pub const EITR0: usize = 0x1680;
    pub const IVAR0: usize = 0x1700;
    pub const IVAR_MISC: usize = 0x1740;
    pub const RDBAL: usize = 0x2800;
    pub const RDBAH: usize = 0x2804;
    pub const RDLEN: usize = 0x2808;
    pub const SRRCTL: usize = 0x280c;
    pub const RDH: usize = 0x2810;
    pub const RDT: usize = 0x2818;
    pub const RXDCTL: usize = 0x2828;
    pub const TDBAL: usize = 0x3800;
    pub const TDBAH: usize = 0x3804;
    pub const TDLEN: usize = 0x3808;
    pub const TDH: usize = 0x3810;
    pub const TDT: usize = 0x3818;
    pub const TXDCTL: usize = 0x3828;
    pub const CRCERRS: usize = 0x4000;
    pub const MPC: usize = 0x4010;
    pub const RXCSUM: usize = 0x5000;
    pub const MTA: usize = 0x5200;
    pub const MTA_LEN: usize = 128;
    pub const RAL0: usize = 0x5400;
    pub const RAH0: usize = 0x5404;
    /// Wake-up control: cleared, no wake-up from this driver.
    pub const WUC: usize = 0x5800;
    /// The BAR must reach the last register used (the I225's `ICR` block
    /// is below this).
    pub const SPAN: usize = WUC + 4;

    pub mod ctrl {
        pub const GIO_MASTER_DISABLE: u32 = 1 << 2;
        pub const SET_LINK_UP: u32 = 1 << 6;
        pub const FORCE_SPEED: u32 = 1 << 11;
        pub const FORCE_DUPLEX: u32 = 1 << 12;
        pub const RESET: u32 = 1 << 26;
        pub const DEVICE_RESET: u32 = 1 << 29;
    }

    pub mod status {
        pub const FULL_DUPLEX: u32 = 1 << 0;
        pub const LINK_UP: u32 = 1 << 1;
        pub const SPEED_SHIFT: u32 = 6;
        pub const SPEED_MASK: u32 = 3 << SPEED_SHIFT;
        pub const GIO_MASTER_ENABLE: u32 = 1 << 19;
        /// The I225 at 2.5 Gb/s (with the 1000 bits).
        pub const SPEED_2500: u32 = 1 << 22;
    }

    pub mod eecd {
        pub const AUTO_READ_DONE: u32 = 1 << 9;
    }

    pub mod ctrl_ext {
        /// The driver has the device (for the firmware).
        pub const DRIVER_LOADED: u32 = 1 << 28;
    }

    /// Interrupt causes, as in `ICR`, `IMS` and `IMC`.
    pub mod int {
        pub const TX_WRITTEN_BACK: u32 = 1 << 0;
        pub const LINK_STATUS_CHANGE: u32 = 1 << 2;
        pub const RX_MIN_THRESHOLD: u32 = 1 << 4;
        pub const RX_OVERRUN: u32 = 1 << 6;
        pub const RX_WRITTEN_BACK: u32 = 1 << 7;
        pub const ALL: u32 = !0;
    }

    pub mod gpie {
        pub const NSICR: u32 = 1 << 0;
        pub const MSIX_MODE: u32 = 1 << 4;
        pub const EIAME: u32 = 1 << 30;
        pub const PBA_SUPPORT: u32 = 1 << 31;
    }

    pub mod phpm {
        /// The PHY goes link-down by itself (I225): cleared.
        pub const GO_LINK_DOWN: u32 = 1 << 5;
    }

    /// Energy-efficient Ethernet, off (I225): `IPCNFG` bits 2-4, `EEER`
    /// bits 16-18.
    pub const IPCNFG_EEE: u32 = 0b111 << 2;
    pub const EEER_LPI: u32 = 0b111 << 16;
}

/// Receive control: enabled, broadcasts and all multicast (IPv6, as on the
/// 82574, ADR-0043), 2048-byte buffers (`SRRCTL` decides), the CRC
/// stripped, `DTYP` 0.
pub const fn receive_control() -> u32 {
    (1 << 1) | (1 << 4) | (1 << 15) | (1 << 26)
}

/// Transmit control: enabled, short frames padded, collision threshold 15,
/// distance 63, retransmit on late collision.
pub const fn transmit_control() -> u32 {
    (1 << 1) | (1 << 3) | (0x0f << 4) | (0x3f << 12) | (1 << 24)
}

/// `SRRCTL`: one buffer of `buffer` bytes (1 KiB units), advanced
/// descriptors.
pub const fn split_receive_control(buffer: usize) -> u32 {
    ((buffer.div_ceil(1024) as u32) & 0x7f) | (1 << 25)
}

/// The queue enable bit of `RXDCTL` and `TXDCTL`.
pub const QUEUE_ENABLE: u32 = 1 << 25;

/// `RXDCTL`: enabled, prefetch 8, host 8, write-back 4.
pub const fn receive_queue(current: u32) -> u32 {
    (current & 0xfff0_0000) | QUEUE_ENABLE | 8 | (8 << 8) | (4 << 16)
}

/// `TXDCTL`: enabled, prefetch 31, host 1, write-back 1, bit 22 set, in
/// descriptors.
pub const fn transmit_queue() -> u32 {
    QUEUE_ENABLE | 0x1f | (1 << 8) | (1 << 16) | (1 << 22) | (1 << 24)
}

/// MSI-X with one vector (0): `GPIE`, then `IVAR0` (receive and transmit
/// queue 0) and `IVAR_MISC` (link changes and the rest).
pub const GPIE_MSIX: u32 =
    reg::gpie::MSIX_MODE | reg::gpie::EIAME | reg::gpie::PBA_SUPPORT | reg::gpie::NSICR;
pub const IVAR0_VECTOR0: u32 = 0x8080;
pub const IVAR_MISC_VECTOR0: u32 = 0x8000;
/// `EIMS` and `EIAC` for vector 0.
pub const VECTOR0: u32 = 1;

/// The legacy causes when there is no MSI-X: polled.
pub const fn interrupt_mask() -> u32 {
    use reg::int::*;
    TX_WRITTEN_BACK | LINK_STATUS_CHANGE | RX_MIN_THRESHOLD | RX_OVERRUN | RX_WRITTEN_BACK
}

/// PHY registers (clause 22) through `MDIC`.
pub mod mdic {
    pub const CONTROL: u8 = 0;
    pub const AUTONEG_ENABLE: u16 = 1 << 12;
    pub const RESTART_AUTONEG: u16 = 1 << 9;
    pub const POWER_DOWN: u16 = 1 << 11;
    const READY: u32 = 1 << 28;
    const ERROR: u32 = 1 << 30;

    pub fn read(address: u32, register: u8) -> u32 {
        (u32::from(register & 0x1f) << 16) | ((address & 0x1f) << 21) | (2 << 26)
    }

    pub fn write(address: u32, register: u8, value: u16) -> u32 {
        (u32::from(register & 0x1f) << 16) | ((address & 0x1f) << 21) | (1 << 26) | u32::from(value)
    }

    /// `None` while busy; then the value, or `Err` if the access failed or
    /// answered another register.
    pub fn result(mdic: u32, register: u8) -> Option<Result<u16, ()>> {
        if mdic & READY == 0 {
            None
        } else if mdic & ERROR != 0 || (mdic >> 16) & 0x1f != u32::from(register & 0x1f) {
            Some(Err(()))
        } else {
            Some(Ok(mdic as u16))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Down,
    Up { mbps: u16, full_duplex: bool },
}

impl Link {
    pub fn from_status(status: u32) -> Self {
        if status & reg::status::LINK_UP == 0 {
            return Self::Down;
        }
        let mbps = if status & reg::status::SPEED_2500 != 0 {
            2500
        } else {
            match (status & reg::status::SPEED_MASK) >> reg::status::SPEED_SHIFT {
                0 => 10,
                1 => 100,
                _ => 1000,
            }
        };
        Self::Up {
            mbps,
            full_duplex: status & reg::status::FULL_DUPLEX != 0,
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

/// An advanced receive descriptor as the driver posts it: the buffer's
/// address, no header buffer (whose low half holds the status on
/// write-back, so it starts clear).
pub fn rx_posted(address: u64) -> [u64; 2] {
    [address, 0]
}

/// A received frame's write-back: status (bytes 8-11) and length (12-13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxWriteBack {
    pub status: u32,
    pub len: u16,
}

pub mod rx_status {
    pub const DONE: u32 = 1 << 0;
    pub const END_OF_PACKET: u32 = 1 << 1;
    /// Any receive error (CRC, symbol, sequence, carrier extension).
    pub const ERROR: u32 = 1 << 31;
    pub const FRAME_ERRORS: u32 = (1 << 24) | (1 << 25) | (1 << 26) | (1 << 28) | ERROR;
}

impl RxWriteBack {
    pub fn decode(high: u64) -> Self {
        Self {
            status: high as u32,
            len: (high >> 32) as u16,
        }
    }

    pub fn done(self) -> bool {
        self.status & rx_status::DONE != 0
    }
}

/// What to do with a completed receive descriptor (the device is not
/// trusted to write back sane values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxOutcome {
    Frame(usize),
    /// Part of a frame spanning buffers (impossible as configured), or the
    /// rest of one.
    Fragment,
    Bad,
}

/// Frames spanning several buffers are dropped whole: after a fragment,
/// everything up to its end of packet goes too.
#[derive(Default)]
pub struct RxAssembler {
    in_fragment: bool,
}

impl RxAssembler {
    pub fn accept(&mut self, written: RxWriteBack, buffer: usize) -> RxOutcome {
        let end = written.status & rx_status::END_OF_PACKET != 0;
        if self.in_fragment || !end {
            self.in_fragment = !end;
            return RxOutcome::Fragment;
        }
        let len = usize::from(written.len);
        if written.status & rx_status::FRAME_ERRORS != 0 || len < 14 || len > buffer.min(1514) {
            return RxOutcome::Bad;
        }
        RxOutcome::Frame(len)
    }
}

/// An advanced transmit data descriptor for one whole frame: the FCS
/// inserted, status reported.
pub fn tx_frame(address: u64, len: u16) -> [u64; 2] {
    let len = u64::from(len);
    let command = (1 << 24) | (1 << 25) | (1 << 27) | (1 << 29) | (0b0011 << 20);
    [address, len | command | ((len << 14) << 32)]
}

/// The write-back status of a transmit descriptor (bytes 12-15): done.
pub const TX_DONE: u32 = 1 << 0;
pub const TX_STATUS_OFFSET: usize = 12;

#[cfg(test)]
mod tests;
