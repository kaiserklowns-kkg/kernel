//! The Intel 82574L Gigabit Ethernet controller ("e1000e", PCI `8086:10d3`,
//! ADR-0041), host-tested and without allocation:
//!
//! - [`reg`]: the registers the driver uses and their bits (82574 GbE
//!   Controller Family datasheet, §10).
//! - [`RxDescriptor`] and [`TxDescriptor`]: legacy descriptors (§7.1.4,
//!   §7.2.10), encoded and decoded; the device is not trusted to write
//!   sane values back.
//! - [`RxRing`] and [`TxRing`]: the head/tail discipline of the descriptor
//!   rings.
//! - [`nvm`], [`mdic`], [`Link`] and the receive address helpers: reading
//!   the MAC address, talking to the PHY, and decoding link status.
//!
//! The driver (`user/e1000e`) owns the registers and the DMA memory; this
//! crate only computes what to write there and checks what comes back.

#![no_std]

/// Intel's PCI vendor ID and the 82574L's device ID (also what QEMU's
/// `-device e1000e` presents).
pub const VENDOR_INTEL: u16 = 0x8086;
pub const DEVICE_82574L: u16 = 0x10d3;

/// Ethernet frames without FCS: the smallest the device delivers (a
/// header) and the largest without jumbo frames (`RCTL.LPE` stays off).
pub const MIN_FRAME: usize = 14;
pub const MAX_FRAME: usize = 1514;

/// Size of one descriptor, either kind.
pub const DESCRIPTOR_SIZE: usize = 16;

/// Ring lengths (`RDLEN`, `TDLEN`) are multiples of 128 bytes: 8
/// descriptors.
pub const RING_ALIGN: u16 = 8;

/// Registers (offsets into BAR 0) and their bits.
pub mod reg {
    /// Device Control.
    pub const CTRL: usize = 0x0000;
    /// Device Status.
    pub const STATUS: usize = 0x0008;
    /// EEPROM/Flash Control.
    pub const EECD: usize = 0x0010;
    /// EEPROM Read (see [`nvm`](crate::nvm)).
    pub const EERD: usize = 0x0014;
    /// Extended Device Control.
    pub const CTRL_EXT: usize = 0x0018;
    /// MDI Control: PHY access (see [`mdic`](crate::mdic)).
    pub const MDIC: usize = 0x0020;
    /// Interrupt Cause Read (write 1s to clear).
    pub const ICR: usize = 0x00c0;
    /// Interrupt Mask Set / Read.
    pub const IMS: usize = 0x00d0;
    /// Interrupt Mask Clear.
    pub const IMC: usize = 0x00d8;
    /// Extended Interrupt Auto Clear (MSI-X).
    pub const EIAC: usize = 0x00dc;
    /// Interrupt Vector Allocation (MSI-X).
    pub const IVAR: usize = 0x00e4;
    /// Receive Control.
    pub const RCTL: usize = 0x0100;
    /// Transmit Control.
    pub const TCTL: usize = 0x0400;
    /// Transmit Inter-Packet Gap.
    pub const TIPG: usize = 0x0410;
    /// Receive queue 0: descriptor base (low, high), length, head, tail,
    /// control.
    pub const RDBAL: usize = 0x2800;
    pub const RDBAH: usize = 0x2804;
    pub const RDLEN: usize = 0x2808;
    pub const RDH: usize = 0x2810;
    pub const RDT: usize = 0x2818;
    /// Transmit queue 0, likewise.
    pub const TDBAL: usize = 0x3800;
    pub const TDBAH: usize = 0x3804;
    pub const TDLEN: usize = 0x3808;
    pub const TDH: usize = 0x3810;
    pub const TDT: usize = 0x3818;
    pub const TXDCTL: usize = 0x3828;
    /// Transmit Arbitration Count, queue 0.
    pub const TARC0: usize = 0x3840;
    /// Statistics (clear on read): CRC errors, missed packets (no
    /// descriptor or FIFO space), receive length errors.
    pub const CRCERRS: usize = 0x4000;
    pub const MPC: usize = 0x4010;
    pub const RLEC: usize = 0x4040;
    /// Receive Checksum Control.
    pub const RXCSUM: usize = 0x5000;
    /// Multicast Table Array: `MTA_LEN` registers.
    pub const MTA: usize = 0x5200;
    pub const MTA_LEN: usize = 128;
    /// Receive Address 0: the MAC address the device accepts.
    pub const RAL0: usize = 0x5400;
    pub const RAH0: usize = 0x5404;
    /// PCIe control registers set at initialisation.
    pub const GCR: usize = 0x5b00;
    pub const GCR2: usize = 0x5b64;
    /// The last register the driver touches, plus 4: BAR 0 must be at
    /// least this large.
    pub const SPAN: usize = GCR2 + 4;

    pub mod ctrl {
        pub const GIO_MASTER_DISABLE: u32 = 1 << 2;
        pub const LINK_RESET: u32 = 1 << 3;
        pub const SET_LINK_UP: u32 = 1 << 6;
        pub const INVERT_LOSS_OF_SIGNAL: u32 = 1 << 7;
        pub const FORCE_SPEED: u32 = 1 << 11;
        pub const FORCE_DUPLEX: u32 = 1 << 12;
        pub const RESET: u32 = 1 << 26;
        /// Reserved; cleared at initialisation (Intel's 82574 init).
        pub const RESERVED_29: u32 = 1 << 29;
        pub const PHY_RESET: u32 = 1 << 31;
    }

    pub mod status {
        pub const FULL_DUPLEX: u32 = 1 << 0;
        pub const LINK_UP: u32 = 1 << 1;
        pub const SPEED_SHIFT: u32 = 6;
        pub const SPEED_MASK: u32 = 3 << SPEED_SHIFT;
        pub const GIO_MASTER_ENABLE: u32 = 1 << 19;
    }

    pub mod eecd {
        /// The NVM was read into the registers after reset.
        pub const AUTO_READ_DONE: u32 = 1 << 9;
    }

    pub mod ctrl_ext {
        /// Set and cleared at initialisation (Intel's 82574 init).
        pub const RESERVED_22: u32 = 1 << 22;
        pub const RESERVED_23: u32 = 1 << 23;
        /// MSI-X pending bit array support.
        pub const PBA_SUPPORT: u32 = 1 << 31;
    }

    /// Interrupt causes, as in `ICR`, `IMS` and `IMC`.
    pub mod int {
        pub const TX_WRITTEN_BACK: u32 = 1 << 0;
        pub const LINK_STATUS_CHANGE: u32 = 1 << 2;
        pub const RX_MIN_THRESHOLD: u32 = 1 << 4;
        pub const RX_OVERRUN: u32 = 1 << 6;
        pub const RX_TIMER: u32 = 1 << 7;
        /// MSI-X: receive queue 0, transmit queue 0, other causes.
        pub const RX_QUEUE0: u32 = 1 << 20;
        pub const TX_QUEUE0: u32 = 1 << 22;
        pub const OTHER: u32 = 1 << 24;
        pub const ASSERTED: u32 = 1 << 31;
        pub const ALL: u32 = !0;
    }

    pub mod rctl {
        pub const ENABLE: u32 = 1 << 1;
        pub const BROADCAST_ACCEPT: u32 = 1 << 15;
        /// `BSIZE` 00 with `BSEX` 0: 2048-byte buffers.
        pub const BUFFER_2048: u32 = 0;
        pub const STRIP_CRC: u32 = 1 << 26;
    }

    pub mod tctl {
        pub const ENABLE: u32 = 1 << 1;
        pub const PAD_SHORT_PACKETS: u32 = 1 << 3;
        pub const COLLISION_THRESHOLD_SHIFT: u32 = 4;
        pub const COLLISION_DISTANCE_SHIFT: u32 = 12;
        pub const RETRANSMIT_ON_LATE_COLLISION: u32 = 1 << 24;
    }

    pub mod txdctl {
        pub const WTHRESH_MASK: u32 = 0x3f << 16;
        /// Write back every descriptor (`WTHRESH` 1), counted in
        /// descriptors (`GRAN`); bit 22 must be set on the 82574.
        pub const WRITE_BACK_EACH: u32 = (1 << 16) | (1 << 24);
        pub const COUNT_DESCRIPTORS: u32 = 1 << 22;
    }

    pub mod rah {
        /// Address Valid: the entry is used for filtering.
        pub const VALID: u32 = 1 << 31;
    }
}

/// The receive control value: enabled, broadcasts accepted, 2048-byte
/// buffers, CRC stripped, legacy descriptors, no promiscuous or multicast
/// modes, no long frames, no loopback.
pub const fn receive_control() -> u32 {
    reg::rctl::ENABLE | reg::rctl::BROADCAST_ACCEPT | reg::rctl::BUFFER_2048 | reg::rctl::STRIP_CRC
}

/// The transmit control value: enabled, short frames padded, the
/// recommended collision threshold (15) and full-duplex collision distance
/// (63).
pub const fn transmit_control() -> u32 {
    reg::tctl::ENABLE
        | reg::tctl::PAD_SHORT_PACKETS
        | (0x0f << reg::tctl::COLLISION_THRESHOLD_SHIFT)
        | (0x3f << reg::tctl::COLLISION_DISTANCE_SHIFT)
        | reg::tctl::RETRANSMIT_ON_LATE_COLLISION
}

/// The inter-packet gap for copper links: IPGT 8, IPGR1 8, IPGR2 6.
pub const TRANSMIT_IPG: u32 = 8 | (8 << 10) | (6 << 20);

/// The interrupt causes the driver enables. With MSI-X the device reports
/// received frames as `RX_QUEUE0`, completed transmissions as `TX_QUEUE0`
/// and everything else (link changes, overruns) as `OTHER`; without it,
/// as the legacy causes.
pub const fn interrupt_mask(msix: bool) -> u32 {
    use reg::int::*;
    let other = LINK_STATUS_CHANGE | RX_OVERRUN;
    if msix {
        RX_QUEUE0 | TX_QUEUE0 | OTHER | other
    } else {
        RX_TIMER | TX_WRITTEN_BACK | RX_MIN_THRESHOLD | other
    }
}

/// `IVAR` routing receive queue 0, transmit queue 0 and other causes to
/// MSI-X `vector` (0–4), with an interrupt on every transmit write-back.
pub const fn ivar(vector: u8) -> u32 {
    let entry = 0x8 | (vector as u32 & 0x7);
    entry | (entry << 8) | (entry << 16) | (1 << 31)
}

// ---- Receive addresses -----------------------------------------------------

pub type Mac = [u8; 6];

/// A MAC address usable as the station's own: unicast and not all zeros.
pub fn is_valid_station(mac: &Mac) -> bool {
    mac[0] & 1 == 0 && mac.iter().any(|&b| b != 0)
}

/// The station address in receive address 0, if it is marked valid (the
/// device loads it from the NVM at reset) and usable.
pub fn mac_from_receive_address(low: u32, high: u32) -> Option<Mac> {
    if high & reg::rah::VALID == 0 {
        return None;
    }
    let [a, b, c, d] = low.to_le_bytes();
    let [e, f, _, _] = high.to_le_bytes();
    let mac = [a, b, c, d, e, f];
    is_valid_station(&mac).then_some(mac)
}

/// `RAL0` and `RAH0` for `mac`, marked valid.
pub fn receive_address(mac: &Mac) -> (u32, u32) {
    let low = u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]);
    let high = u32::from_le_bytes([mac[4], mac[5], 0, 0]) | reg::rah::VALID;
    (low, high)
}

/// The NVM (EEPROM or flash behind `EERD`, §5.6, §6.1).
pub mod nvm {
    use super::Mac;

    pub const START: u32 = 1 << 0;
    pub const DONE: u32 = 1 << 1;
    pub const ADDRESS_SHIFT: u32 = 2;
    pub const DATA_SHIFT: u32 = 16;

    /// Words covered by the checksum, which sum to [`CHECKSUM`].
    pub const CHECKSUM_WORDS: usize = 0x40;
    pub const CHECKSUM: u16 = 0xbaba;

    /// The `EERD` value that reads `word`.
    pub fn read_request(word: u16) -> u32 {
        (u32::from(word) << ADDRESS_SHIFT) | START
    }

    /// The word read, once `EERD` shows the read done.
    pub fn read_result(eerd: u32) -> Option<u16> {
        (eerd & DONE != 0).then_some((eerd >> DATA_SHIFT) as u16)
    }

    /// Whether the checksummed words sum to [`CHECKSUM`].
    pub fn checksum_valid(words: &[u16; CHECKSUM_WORDS]) -> bool {
        words.iter().fold(0u16, |sum, &w| sum.wrapping_add(w)) == CHECKSUM
    }

    /// The MAC address in words 0–2 (little-endian byte pairs).
    pub fn mac(words: &[u16]) -> Option<Mac> {
        let [a, b, c] = words.get(..3)?.try_into().ok()?;
        let ([m0, m1], [m2, m3], [m4, m5]) = (
            u16::to_le_bytes(a),
            u16::to_le_bytes(b),
            u16::to_le_bytes(c),
        );
        let mac = [m0, m1, m2, m3, m4, m5];
        super::is_valid_station(&mac).then_some(mac)
    }
}

/// PHY registers through `MDIC` (§10.2.2.6, §10.3). The 82574's internal
/// PHY is at MDIO address 1.
pub mod mdic {
    pub const PHY_ADDRESS: u32 = 1;
    const REGISTER_SHIFT: u32 = 16;
    const PHY_SHIFT: u32 = 21;
    const OP_WRITE: u32 = 1 << 26;
    const OP_READ: u32 = 2 << 26;
    const READY: u32 = 1 << 28;
    const ERROR: u32 = 1 << 30;

    /// PHY control register and its bits.
    pub const PHY_CONTROL: u8 = 0;
    pub const RESTART_AUTONEG: u16 = 1 << 9;
    pub const AUTONEG_ENABLE: u16 = 1 << 12;

    fn command(register: u8, op: u32) -> u32 {
        (u32::from(register & 0x1f) << REGISTER_SHIFT) | (PHY_ADDRESS << PHY_SHIFT) | op
    }

    /// The `MDIC` value that reads PHY `register`.
    pub fn read(register: u8) -> u32 {
        command(register, OP_READ)
    }

    /// The `MDIC` value that writes `value` to PHY `register`.
    pub fn write(register: u8, value: u16) -> u32 {
        command(register, OP_WRITE) | u32::from(value)
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum State {
        Busy,
        /// Done; for a read, the value.
        Done(u16),
        Failed,
    }

    pub fn state(mdic: u32) -> State {
        if mdic & READY == 0 {
            State::Busy
        } else if mdic & ERROR != 0 {
            State::Failed
        } else {
            State::Done(mdic as u16)
        }
    }
}

// ---- Link ------------------------------------------------------------------

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
        let mbps = match (status & reg::status::SPEED_MASK) >> reg::status::SPEED_SHIFT {
            0 => 10,
            1 => 100,
            _ => 1000,
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

// ---- Descriptors -----------------------------------------------------------

/// A legacy receive descriptor (§7.1.4): the driver writes the buffer
/// address; the device writes the rest back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RxDescriptor {
    pub address: u64,
    pub length: u16,
    pub checksum: u16,
    pub status: u8,
    pub errors: u8,
    pub special: u16,
}

/// Receive descriptor status bits.
pub mod rx_status {
    pub const DONE: u8 = 1 << 0;
    pub const END_OF_PACKET: u8 = 1 << 1;
}

/// Receive descriptor error bits that mean the frame is bad: CRC or
/// alignment, symbol, sequence, carrier extension, data errors. (TCP/UDP
/// and IP checksum errors, bits 5 and 6, apply only with checksum offload,
/// which stays off: the stack checks its own.)
pub const RX_FRAME_ERRORS: u8 = 0b1001_0111;

/// Byte offset of the status byte in a descriptor (either kind).
pub const STATUS_OFFSET: usize = 12;

impl RxDescriptor {
    /// A descriptor handing the buffer at `address` to the device.
    pub fn posted(address: u64) -> Self {
        Self {
            address,
            ..Self::default()
        }
    }

    pub fn encode(&self) -> [u8; DESCRIPTOR_SIZE] {
        let mut out = [0; DESCRIPTOR_SIZE];
        out[..8].copy_from_slice(&self.address.to_le_bytes());
        out[8..10].copy_from_slice(&self.length.to_le_bytes());
        out[10..12].copy_from_slice(&self.checksum.to_le_bytes());
        out[12] = self.status;
        out[13] = self.errors;
        out[14..].copy_from_slice(&self.special.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8; DESCRIPTOR_SIZE]) -> Self {
        Self {
            address: u64::from_le_bytes(bytes[..8].try_into().expect("8 bytes")),
            length: u16::from_le_bytes([bytes[8], bytes[9]]),
            checksum: u16::from_le_bytes([bytes[10], bytes[11]]),
            status: bytes[12],
            errors: bytes[13],
            special: u16::from_le_bytes([bytes[14], bytes[15]]),
        }
    }

    pub fn done(&self) -> bool {
        self.status & rx_status::DONE != 0
    }

    /// What a completed descriptor holds, for a buffer of `buffer_len`
    /// bytes. Lengths come from the device and are checked.
    pub fn outcome(&self, buffer_len: usize) -> RxOutcome {
        let len = usize::from(self.length);
        if self.status & rx_status::END_OF_PACKET == 0 {
            RxOutcome::Fragment
        } else if self.errors & RX_FRAME_ERRORS != 0 {
            RxOutcome::Error(self.errors)
        } else if !(MIN_FRAME..=MAX_FRAME.min(buffer_len)).contains(&len) {
            RxOutcome::BadLength(self.length)
        } else {
            RxOutcome::Frame(len)
        }
    }
}

/// A completed receive descriptor, judged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxOutcome {
    /// A whole, good frame of this many bytes.
    Frame(usize),
    /// Part of a frame spanning several buffers: it cannot happen with
    /// 2048-byte buffers and long frames off, so the frame is dropped
    /// (this and every descriptor up to its end).
    Fragment,
    /// A frame the device flagged (the error bits).
    Error(u8),
    /// A runt, or longer than allowed or than its buffer.
    BadLength(u16),
}

/// Assembles frames from completed descriptors, dropping any frame that
/// spans several (see [`RxOutcome::Fragment`]) through its last one.
#[derive(Clone, Copy, Debug, Default)]
pub struct RxAssembler {
    discarding: bool,
}

impl RxAssembler {
    /// The frame in a completed descriptor: `Ok(len)` to deliver, or the
    /// reason to drop it.
    pub fn accept(
        &mut self,
        descriptor: &RxDescriptor,
        buffer_len: usize,
    ) -> Result<usize, RxOutcome> {
        let outcome = descriptor.outcome(buffer_len);
        if self.discarding {
            // The tail of a dropped multi-buffer frame.
            self.discarding = outcome == RxOutcome::Fragment;
            return Err(RxOutcome::Fragment);
        }
        match outcome {
            RxOutcome::Frame(len) => Ok(len),
            RxOutcome::Fragment => {
                self.discarding = true;
                Err(outcome)
            }
            other => Err(other),
        }
    }
}

/// A legacy transmit descriptor (§7.2.10).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxDescriptor {
    pub address: u64,
    pub length: u16,
    pub cso: u8,
    pub command: u8,
    pub status: u8,
    pub css: u8,
    pub special: u16,
}

/// Transmit descriptor command bits.
pub mod tx_command {
    pub const END_OF_PACKET: u8 = 1 << 0;
    pub const INSERT_FCS: u8 = 1 << 1;
    pub const REPORT_STATUS: u8 = 1 << 3;
}

/// Transmit descriptor status bits.
pub mod tx_status {
    pub const DONE: u8 = 1 << 0;
    pub const EXCESS_COLLISIONS: u8 = 1 << 1;
    pub const LATE_COLLISION: u8 = 1 << 2;
}

impl TxDescriptor {
    /// One whole frame in one buffer: end of packet, FCS added by the
    /// device, status reported (`DONE` set) when sent.
    pub fn frame(address: u64, len: u16) -> Self {
        Self {
            address,
            length: len,
            command: tx_command::END_OF_PACKET | tx_command::INSERT_FCS | tx_command::REPORT_STATUS,
            ..Self::default()
        }
    }

    pub fn encode(&self) -> [u8; DESCRIPTOR_SIZE] {
        let mut out = [0; DESCRIPTOR_SIZE];
        out[..8].copy_from_slice(&self.address.to_le_bytes());
        out[8..10].copy_from_slice(&self.length.to_le_bytes());
        out[10] = self.cso;
        out[11] = self.command;
        out[12] = self.status;
        out[13] = self.css;
        out[14..].copy_from_slice(&self.special.to_le_bytes());
        out
    }

    /// The two little-endian words of [`encode`](Self::encode), for
    /// writing to DMA memory as whole words.
    pub fn words(&self) -> [u64; 2] {
        let bytes = self.encode();
        [
            u64::from_le_bytes(bytes[..8].try_into().expect("8 bytes")),
            u64::from_le_bytes(bytes[8..].try_into().expect("8 bytes")),
        ]
    }

    /// Transmission errors in a written-back status byte (collisions only
    /// happen at half duplex).
    pub fn failed(status: u8) -> bool {
        status & (tx_status::EXCESS_COLLISIONS | tx_status::LATE_COLLISION) != 0
    }
}

// ---- Rings -----------------------------------------------------------------

/// A valid ring size: at least 8 descriptors, a multiple of 8, and small
/// enough for indices to stay in `u16`.
pub const fn valid_ring_size(size: u16) -> bool {
    size >= RING_ALIGN && size.is_multiple_of(RING_ALIGN) && size <= 4096
}

/// The receive ring (§7.1.6). Every buffer but one is the device's: the
/// device fills descriptors from its head up to (not including) the tail
/// `RDT`, and head == tail means it has none.
///
/// The driver consumes completed descriptors in ring order, from
/// [`next`](Self::next). Each one consumed becomes the ring's unposted
/// descriptor (the new tail), which gives the previous unposted one back
/// to the device. A completed descriptor waits, holding its frame, until
/// the driver consumes it: a stack that does not keep up makes the device
/// drop frames (counted in `MPC`) rather than the driver queue them.
#[derive(Clone, Copy, Debug)]
pub struct RxRing {
    size: u16,
    next: u16,
}

impl RxRing {
    /// `size` descriptors (see [`valid_ring_size`]).
    pub const fn new(size: u16) -> Self {
        assert!(valid_ring_size(size));
        Self { size, next: 0 }
    }

    pub fn size(&self) -> u16 {
        self.size
    }

    /// The next descriptor the device completes.
    pub fn next(&self) -> u16 {
        self.next
    }

    /// `RDT` at start: all descriptors but the last posted.
    pub fn initial_tail(&self) -> u16 {
        self.size - 1
    }

    /// The driver is done with descriptor [`next`](Self::next) (its frame
    /// copied out or dropped) and has re-armed it; returns the new `RDT`.
    pub fn consume(&mut self) -> u16 {
        let tail = self.next;
        self.next = (self.next + 1) % self.size;
        tail
    }
}

/// The transmit ring (§7.2.3): the driver fills descriptors at the tail
/// (`TDT`); the device sends from its head and sets `DONE` in each (status
/// is reported for every frame). Descriptors from [`oldest`](Self::oldest)
/// to the tail are the device's; one is always left empty, so a full ring
/// is never mistaken for an empty one.
#[derive(Clone, Copy, Debug)]
pub struct TxRing {
    size: u16,
    clean: u16,
    tail: u16,
}

impl TxRing {
    /// `size` descriptors (see [`valid_ring_size`]).
    pub const fn new(size: u16) -> Self {
        assert!(valid_ring_size(size));
        Self {
            size,
            clean: 0,
            tail: 0,
        }
    }

    pub fn size(&self) -> u16 {
        self.size
    }

    /// Descriptors handed to the device and not yet reclaimed.
    pub fn in_flight(&self) -> u16 {
        (self.tail + self.size - self.clean) % self.size
    }

    /// Descriptors that can still be filled.
    pub fn free(&self) -> u16 {
        self.size - 1 - self.in_flight()
    }

    /// A descriptor to fill, if one is free: after filling it, write the
    /// new [`tail`](Self::tail) to `TDT`.
    pub fn claim(&mut self) -> Option<u16> {
        if self.free() == 0 {
            return None;
        }
        let index = self.tail;
        self.tail = (self.tail + 1) % self.size;
        Some(index)
    }

    /// The value for `TDT`.
    pub fn tail(&self) -> u16 {
        self.tail
    }

    /// The oldest descriptor still the device's, if any: reclaim it (with
    /// [`complete`](Self::complete)) once its status shows `DONE`.
    pub fn oldest(&self) -> Option<u16> {
        (self.in_flight() > 0).then_some(self.clean)
    }

    /// The oldest descriptor is done.
    pub fn complete(&mut self) {
        if self.in_flight() > 0 {
            self.clean = (self.clean + 1) % self.size;
        }
    }
}

#[cfg(test)]
mod tests;
