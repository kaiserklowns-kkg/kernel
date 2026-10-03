//! xHCI data structures (xHCI 1.2): transfer request blocks, the cycle-bit
//! ring discipline, and device contexts. The driver owns the memory; these
//! types only encode and decode it.

use crate::Speed;

// ---- Registers -----------------------------------------------------------

/// Capability registers (§5.3).
pub mod cap {
    pub const CAPLENGTH: usize = 0x00;
    pub const HCSPARAMS1: usize = 0x04;
    pub const HCSPARAMS2: usize = 0x08;
    pub const HCCPARAMS1: usize = 0x10;
    pub const DBOFF: usize = 0x14;
    pub const RTSOFF: usize = 0x18;
}

/// Operational registers (§5.4), from `CAPLENGTH`.
pub mod op {
    pub const USBCMD: usize = 0x00;
    pub const USBSTS: usize = 0x04;
    pub const PAGESIZE: usize = 0x08;
    pub const CRCR: usize = 0x18;
    pub const DCBAAP: usize = 0x30;
    pub const CONFIG: usize = 0x38;
    /// Port register sets: `PORTSC` of port `n` (1-based) is at
    /// `PORTS + 0x10 * (n - 1)`.
    pub const PORTS: usize = 0x400;

    pub const CMD_RUN: u32 = 1 << 0;
    pub const CMD_RESET: u32 = 1 << 1;
    pub const CMD_INTERRUPTS: u32 = 1 << 2;

    pub const STS_HALTED: u32 = 1 << 0;
    pub const STS_FATAL: u32 = 1 << 2;
    pub const STS_EVENT: u32 = 1 << 3;
    pub const STS_NOT_READY: u32 = 1 << 11;
}

/// Interrupter 0's registers (§5.5.2), from `RTSOFF + 0x20`.
pub mod interrupter {
    pub const IMAN: usize = 0x00;
    pub const IMOD: usize = 0x04;
    pub const ERSTSZ: usize = 0x08;
    pub const ERSTBA: usize = 0x10;
    pub const ERDP: usize = 0x18;

    pub const IMAN_PENDING: u32 = 1 << 0;
    pub const IMAN_ENABLE: u32 = 1 << 1;
    /// `ERDP`: event handler busy (write 1 to clear).
    pub const ERDP_BUSY: u64 = 1 << 3;
}

/// `PORTSC` bits (§5.4.8).
pub mod port {
    pub const CONNECTED: u32 = 1 << 0;
    pub const ENABLED: u32 = 1 << 1;
    pub const RESET: u32 = 1 << 4;
    pub const POWER: u32 = 1 << 9;
    pub const SPEED_SHIFT: u32 = 10;
    /// Change bits, all write-1-to-clear: connect, enable, warm reset,
    /// over-current, reset, link state, config error.
    pub const CHANGES: u32 = 0x7f << 17;
    pub const CONNECT_CHANGE: u32 = 1 << 17;
    pub const RESET_CHANGE: u32 = 1 << 21;
    /// Bits a write must carry over unchanged (power, indicator, wake
    /// enables); everything else written as 0 is harmless.
    const PRESERVE: u32 = (1 << 9) | (3 << 14) | (7 << 25);

    /// The value to write to `PORTSC` (read as `current`) to set `set`
    /// without clearing a change bit or disabling the port by accident.
    pub const fn write_value(current: u32, set: u32) -> u32 {
        (current & PRESERVE) | set
    }

    pub const fn speed(portsc: u32) -> u8 {
        ((portsc >> SPEED_SHIFT) & 0xf) as u8
    }
}

/// Parsed `HCSPARAMS1`/`HCSPARAMS2`/`HCCPARAMS1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    pub max_slots: u8,
    pub max_interrupters: u16,
    pub max_ports: u8,
    pub scratchpads: u16,
    pub context_size: usize,
    pub extended_capabilities: usize,
}

impl Params {
    pub fn decode(hcsparams1: u32, hcsparams2: u32, hccparams1: u32) -> Self {
        let scratch_hi = (hcsparams2 >> 21) & 0x1f;
        let scratch_lo = (hcsparams2 >> 27) & 0x1f;
        Self {
            max_slots: hcsparams1 as u8,
            max_interrupters: ((hcsparams1 >> 8) & 0x7ff) as u16,
            max_ports: (hcsparams1 >> 24) as u8,
            scratchpads: (scratch_hi << 5 | scratch_lo) as u16,
            context_size: if hccparams1 & (1 << 2) != 0 { 64 } else { 32 },
            extended_capabilities: ((hccparams1 >> 16) as usize) * 4,
        }
    }
}

/// Extended capability IDs (§7).
pub const EXT_LEGACY: u8 = 1;
/// USB Legacy Support: the firmware's and the OS's ownership semaphores.
pub const LEGACY_BIOS_OWNED: u32 = 1 << 16;
pub const LEGACY_OS_OWNED: u32 = 1 << 24;

// ---- TRBs ----------------------------------------------------------------

pub const TRB_SIZE: usize = 16;

/// TRB types (§6.4.6).
pub mod trb_type {
    pub const NORMAL: u8 = 1;
    pub const SETUP: u8 = 2;
    pub const DATA: u8 = 3;
    pub const STATUS: u8 = 4;
    pub const LINK: u8 = 6;
    pub const ENABLE_SLOT: u8 = 9;
    pub const DISABLE_SLOT: u8 = 10;
    pub const ADDRESS_DEVICE: u8 = 11;
    pub const CONFIGURE_ENDPOINT: u8 = 12;
    pub const EVALUATE_CONTEXT: u8 = 13;
    pub const RESET_ENDPOINT: u8 = 14;
    pub const STOP_ENDPOINT: u8 = 15;
    pub const SET_TR_DEQUEUE: u8 = 16;
    pub const NO_OP: u8 = 23;
    pub const TRANSFER_EVENT: u8 = 32;
    pub const COMMAND_COMPLETION: u8 = 33;
    pub const PORT_STATUS_CHANGE: u8 = 34;
    pub const HOST_CONTROLLER: u8 = 37;
}

/// Completion codes (§6.4.5).
pub mod completion {
    pub const SUCCESS: u8 = 1;
    pub const STALL: u8 = 6;
    pub const SHORT_PACKET: u8 = 13;
}

const CYCLE: u32 = 1 << 0;
const TOGGLE_CYCLE: u32 = 1 << 1;
const INTERRUPT_ON_SHORT: u32 = 1 << 2;
const CHAIN: u32 = 1 << 4;
const INTERRUPT_ON_COMPLETION: u32 = 1 << 5;
const IMMEDIATE_DATA: u32 = 1 << 6;
const DIRECTION_IN: u32 = 1 << 16;

/// One TRB, as four little-endian dwords. The cycle bit is set by the ring
/// when the TRB is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Trb(pub [u32; 4]);

impl Trb {
    fn new(parameter: u64, status: u32, kind: u8, flags: u32) -> Self {
        Self([
            parameter as u32,
            (parameter >> 32) as u32,
            status,
            u32::from(kind) << 10 | flags,
        ])
    }

    pub fn kind(&self) -> u8 {
        ((self.0[3] >> 10) & 0x3f) as u8
    }

    pub fn cycle(&self) -> bool {
        self.0[3] & CYCLE != 0
    }

    pub fn parameter(&self) -> u64 {
        u64::from(self.0[0]) | u64::from(self.0[1]) << 32
    }

    pub fn with_cycle(mut self, cycle: bool) -> Self {
        self.0[3] = (self.0[3] & !CYCLE) | u32::from(cycle);
        self
    }

    pub fn to_bytes(self) -> [u8; TRB_SIZE] {
        let mut out = [0u8; TRB_SIZE];
        for (chunk, dword) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.0) {
            chunk.copy_from_slice(&dword.to_le_bytes());
        }
        out
    }

    pub fn from_bytes(bytes: &[u8; TRB_SIZE]) -> Self {
        let dword =
            |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        Self([dword(0), dword(4), dword(8), dword(12)])
    }

    // -- Transfer TRBs --

    /// Setup stage of a control transfer: the packet as immediate data.
    /// `data`: whether a data stage follows, and its direction.
    pub fn setup(packet: [u8; 8], data: Option<bool>) -> Self {
        let transfer_type = match data {
            None => 0,
            Some(false) => 2,
            Some(true) => 3,
        };
        Self::new(
            u64::from_le_bytes(packet),
            8,
            trb_type::SETUP,
            IMMEDIATE_DATA | transfer_type << 16,
        )
    }

    /// Data stage; a short packet raises an event, which tells the length.
    pub fn data(buffer: u64, len: u32, input: bool) -> Self {
        Self::new(
            buffer,
            len & 0x1_ffff,
            trb_type::DATA,
            INTERRUPT_ON_SHORT | if input { DIRECTION_IN } else { 0 },
        )
    }

    /// Status stage: in the direction opposite the data stage (IN when
    /// there was none), interrupting on completion.
    pub fn status(input: bool) -> Self {
        Self::new(
            0,
            0,
            trb_type::STATUS,
            INTERRUPT_ON_COMPLETION | if input { DIRECTION_IN } else { 0 },
        )
    }

    /// A bulk or interrupt transfer of `len` bytes at `buffer`.
    pub fn normal(buffer: u64, len: u32) -> Self {
        Self::new(
            buffer,
            len & 0x1_ffff,
            trb_type::NORMAL,
            INTERRUPT_ON_COMPLETION | INTERRUPT_ON_SHORT,
        )
    }

    /// Part of a multi-TRB transfer: chained to the next, which
    /// completes the transfer (only the last interrupts on completion).
    pub fn chained(mut self) -> Self {
        self.0[3] = (self.0[3] & !INTERRUPT_ON_COMPLETION) | CHAIN;
        self
    }

    pub fn link(target: u64) -> Self {
        Self::new(target, 0, trb_type::LINK, TOGGLE_CYCLE)
    }

    // -- Commands --

    pub fn enable_slot() -> Self {
        Self::new(0, 0, trb_type::ENABLE_SLOT, 0)
    }

    pub fn disable_slot(slot: u8) -> Self {
        Self::new(0, 0, trb_type::DISABLE_SLOT, u32::from(slot) << 24)
    }

    pub fn address_device(input_context: u64, slot: u8) -> Self {
        Self::new(
            input_context,
            0,
            trb_type::ADDRESS_DEVICE,
            u32::from(slot) << 24,
        )
    }

    pub fn configure_endpoint(input_context: u64, slot: u8) -> Self {
        Self::new(
            input_context,
            0,
            trb_type::CONFIGURE_ENDPOINT,
            u32::from(slot) << 24,
        )
    }

    pub fn evaluate_context(input_context: u64, slot: u8) -> Self {
        Self::new(
            input_context,
            0,
            trb_type::EVALUATE_CONTEXT,
            u32::from(slot) << 24,
        )
    }

    /// Clears a halted endpoint's state in the controller.
    pub fn reset_endpoint(slot: u8, dci: u8) -> Self {
        Self::new(
            0,
            0,
            trb_type::RESET_ENDPOINT,
            u32::from(slot) << 24 | u32::from(dci) << 16,
        )
    }

    pub fn stop_endpoint(slot: u8, dci: u8) -> Self {
        Self::new(
            0,
            0,
            trb_type::STOP_ENDPOINT,
            u32::from(slot) << 24 | u32::from(dci) << 16,
        )
    }

    /// Moves a stopped or reset endpoint's dequeue pointer, abandoning the
    /// TRBs before it.
    pub fn set_tr_dequeue(slot: u8, dci: u8, address: u64, cycle: bool) -> Self {
        Self::new(
            address | u64::from(cycle),
            0,
            trb_type::SET_TR_DEQUEUE,
            u32::from(slot) << 24 | u32::from(dci) << 16,
        )
    }

    pub fn no_op() -> Self {
        Self::new(0, 0, trb_type::NO_OP, 0)
    }
}

/// A decoded event TRB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// `trb`: the transfer TRB it completes; `residue`: bytes not
    /// transferred.
    Transfer {
        slot: u8,
        endpoint: u8,
        trb: u64,
        code: u8,
        residue: u32,
    },
    Command {
        trb: u64,
        code: u8,
        slot: u8,
    },
    PortStatus {
        port: u8,
    },
    HostController {
        code: u8,
    },
    Other(u8),
}

impl Event {
    pub fn decode(trb: &Trb) -> Self {
        let code = (trb.0[2] >> 24) as u8;
        let slot = (trb.0[3] >> 24) as u8;
        match trb.kind() {
            trb_type::TRANSFER_EVENT => Self::Transfer {
                slot,
                endpoint: ((trb.0[3] >> 16) & 0x1f) as u8,
                trb: trb.parameter(),
                code,
                residue: trb.0[2] & 0xff_ffff,
            },
            trb_type::COMMAND_COMPLETION => Self::Command {
                trb: trb.parameter(),
                code,
                slot,
            },
            trb_type::PORT_STATUS_CHANGE => Self::PortStatus {
                port: (trb.0[0] >> 24) as u8,
            },
            trb_type::HOST_CONTROLLER => Self::HostController { code },
            kind => Self::Other(kind),
        }
    }
}

// ---- Rings ---------------------------------------------------------------

/// The producer side of a command or transfer ring of `size` TRBs, the
/// last of which is a link back to the start (§4.9.2).
#[derive(Clone, Copy, Debug)]
pub struct Producer {
    size: u16,
    index: u16,
    cycle: bool,
}

impl Producer {
    /// `size` TRBs, at least 2.
    pub const fn new(size: u16) -> Self {
        assert!(size >= 2);
        Self {
            size,
            index: 0,
            cycle: true,
        }
    }

    /// The cycle state to give the controller with the ring's address.
    pub fn cycle(&self) -> bool {
        self.cycle
    }

    /// Where the next pushed TRB goes.
    pub fn index(&self) -> u16 {
        self.index
    }

    /// Where the next TRB goes, and its cycle bit. Calls `write(index,
    /// trb)` for the TRB and, at the end of the ring, for the link TRB
    /// (`link` built for the ring's start).
    pub fn push(&mut self, trb: Trb, link: Trb, mut write: impl FnMut(u16, Trb)) -> u16 {
        let at = self.index;
        write(at, trb.with_cycle(self.cycle));
        self.index += 1;
        if self.index == self.size - 1 {
            write(self.index, link.with_cycle(self.cycle));
            self.index = 0;
            self.cycle = !self.cycle;
        }
        at
    }
}

/// The consumer side of the (single-segment) event ring.
#[derive(Clone, Copy, Debug)]
pub struct Consumer {
    size: u16,
    index: u16,
    cycle: bool,
}

impl Consumer {
    pub const fn new(size: u16) -> Self {
        Self {
            size,
            index: 0,
            cycle: true,
        }
    }

    /// The next event if the controller has written it (its cycle bit
    /// matches), advancing past it.
    pub fn pop(&mut self, read: impl FnOnce(u16) -> Trb) -> Option<Trb> {
        let trb = read(self.index);
        if trb.cycle() != self.cycle {
            return None;
        }
        self.index += 1;
        if self.index == self.size {
            self.index = 0;
            self.cycle = !self.cycle;
        }
        Some(trb)
    }

    /// The index of the next event to read (for `ERDP`).
    pub fn index(&self) -> u16 {
        self.index
    }
}

// ---- Contexts ------------------------------------------------------------

/// Endpoint types in endpoint contexts (§6.2.3).
pub mod endpoint_type {
    pub const CONTROL: u32 = 4;
    pub const INTERRUPT_IN: u32 = 7;
    pub const BULK_IN: u32 = 6;
    pub const BULK_OUT: u32 = 2;
}

/// A slot context's contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub speed: Speed,
    /// The root hub port the device is reached through.
    pub root_port: u8,
    /// Hub ports below the root, 4 bits per tier.
    pub route: u32,
    /// The highest endpoint context in use.
    pub last_dci: u8,
    /// Low and full speed devices behind a high-speed hub: that hub's
    /// slot and port (its transaction translator).
    pub tt: Option<(u8, u8)>,
    /// The translating hub has one translator per port.
    pub multi_tt: bool,
    /// Set for hubs.
    pub hub: Option<HubSlot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HubSlot {
    pub ports: u8,
    pub think_time: u8,
}

/// Builds input contexts (§6.2.5) in a buffer of
/// `33 * context_size` bytes: the input control context, the slot context,
/// then endpoint contexts by DCI.
pub struct InputContext<'a> {
    bytes: &'a mut [u8],
    size: usize,
}

impl<'a> InputContext<'a> {
    pub const fn len(context_size: usize) -> usize {
        33 * context_size
    }

    /// Clears `bytes` and wraps it.
    pub fn new(bytes: &'a mut [u8], context_size: usize) -> Self {
        assert!(bytes.len() >= Self::len(context_size));
        bytes.fill(0);
        Self {
            bytes,
            size: context_size,
        }
    }

    fn set(&mut self, context: usize, dword: usize, value: u32) {
        let at = context * self.size + dword * 4;
        self.bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// Marks contexts to add (bit 0: slot, bit 1: endpoint 0, …).
    pub fn add(&mut self, flags: u32) {
        self.set(0, 1, flags);
    }

    /// The slot context (§6.2.2).
    pub fn slot(&mut self, slot: &Slot) {
        let mut dword0 = slot.route & 0xf_ffff
            | u32::from(slot.speed.id()) << 20
            | u32::from(slot.last_dci) << 27;
        if slot.multi_tt {
            dword0 |= 1 << 25;
        }
        let mut dword1 = u32::from(slot.root_port) << 16;
        let mut dword2 = 0;
        if let Some(hub) = slot.hub {
            dword0 |= 1 << 26;
            dword1 |= u32::from(hub.ports) << 24;
            dword2 |= u32::from(hub.think_time & 3) << 16;
        }
        if let Some((tt_slot, tt_port)) = slot.tt {
            dword2 |= u32::from(tt_slot) | u32::from(tt_port) << 8;
        }
        self.set(1, 0, dword0);
        self.set(1, 1, dword1);
        self.set(1, 2, dword2);
    }

    /// Sets an endpoint context's SuperSpeed burst size (packets per
    /// burst, less one).
    pub fn max_burst(&mut self, dci: u8, burst: u8) {
        let context = 1 + usize::from(dci);
        let at = context * self.size + 4;
        let dword = u32::from_le_bytes(self.bytes[at..at + 4].try_into().expect("4 bytes"));
        self.set(context, 1, (dword & !0xff00) | u32::from(burst) << 8);
    }

    /// An endpoint context: `kind` from [`endpoint_type`], `interval` as
    /// the xHCI exponent, the transfer ring's address and cycle state.
    pub fn endpoint(
        &mut self,
        dci: u8,
        kind: u32,
        max_packet: u16,
        interval: u8,
        ring: u64,
        cycle: bool,
    ) {
        let context = 1 + usize::from(dci);
        self.set(context, 0, u32::from(interval) << 16);
        // Three retries on errors (CErr), as recommended.
        self.set(context, 1, 3 << 1 | kind << 3 | u32::from(max_packet) << 16);
        let dequeue = ring | u64::from(cycle);
        self.set(context, 2, dequeue as u32);
        self.set(context, 3, (dequeue >> 32) as u32);
        let average = if kind == endpoint_type::CONTROL {
            8
        } else {
            u32::from(max_packet)
        };
        // Average TRB length; max ESIT payload for periodic endpoints.
        let esit = if kind == endpoint_type::INTERRUPT_IN {
            u32::from(max_packet) << 16
        } else {
            0
        };
        self.set(context, 4, average | esit);
    }
}

/// The xHCI interval exponent for an interrupt endpoint's `bInterval`
/// (§6.2.3.6): in frames (1 ms) at full and low speed, as `2^(n-1)`
/// microframes at high speed and above. The result counts 125 µs
/// microframes as `2^interval`.
pub fn interrupt_interval(speed: Speed, b_interval: u8) -> u8 {
    match speed {
        Speed::Low | Speed::Full => {
            let microframes = u32::from(b_interval.max(1)) * 8;
            // floor(log2), within the 3..=10 the spec allows here.
            (31 - microframes.leading_zeros()).clamp(3, 10) as u8
        }
        _ => b_interval.clamp(1, 16) - 1,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;
    use std::vec::Vec;

    #[test]
    fn decodes_parameters() {
        // QEMU's qemu-xhci: 64 slots, 16 interrupters, 8 ports, no
        // scratchpads, 32-byte contexts, extended capabilities at 0x20.
        let params = Params::decode(0x0800_1040, 0, 0x0008_0001);
        assert_eq!(params.max_slots, 64);
        assert_eq!(params.max_interrupters, 16);
        assert_eq!(params.max_ports, 8);
        assert_eq!(params.scratchpads, 0);
        assert_eq!(params.context_size, 32);
        assert_eq!(params.extended_capabilities, 0x20);
        let params = Params::decode(0, (1 << 21) | (3 << 27), 1 << 2);
        assert_eq!(params.scratchpads, 35);
        assert_eq!(params.context_size, 64);
    }

    #[test]
    fn encodes_trbs() {
        let setup = Trb::setup([0x80, 6, 0, 1, 0, 0, 18, 0], Some(true)).with_cycle(true);
        assert_eq!(setup.kind(), trb_type::SETUP);
        assert_eq!(
            setup.parameter(),
            u64::from_le_bytes([0x80, 6, 0, 1, 0, 0, 18, 0])
        );
        assert_eq!(setup.0[2], 8);
        assert_eq!(setup.0[3], 3 << 16 | 1 << 6 | 2 << 10 | 1);
        assert_eq!(Trb::data(0x1000, 18, true).0[3], 1 << 16 | 3 << 10 | 1 << 2);
        assert_eq!(Trb::status(false).0[3], 1 << 5 | 4 << 10);
        assert_eq!(
            Trb::address_device(0x2000, 5).0,
            [0x2000, 0, 0, 5 << 24 | 11 << 10]
        );
        assert_eq!(Trb::link(0x3000).0[3], 6 << 10 | 1 << 1);
        let trb = Trb::normal(0xdead_beef_0000, 8);
        assert_eq!(Trb::from_bytes(&trb.to_bytes()), trb);
        assert_eq!(trb.chained().0[3], 1 << 10 | 1 << 2 | 1 << 4);
        assert_eq!(Trb::reset_endpoint(2, 5).0[3], 2 << 24 | 5 << 16 | 14 << 10);
        assert_eq!(Trb::stop_endpoint(2, 5).0[3], 2 << 24 | 5 << 16 | 15 << 10);
        assert_eq!(
            Trb::set_tr_dequeue(1, 3, 0x9000, true).0,
            [0x9001, 0, 0, 1 << 24 | 3 << 16 | 16 << 10]
        );
    }

    #[test]
    fn decodes_events() {
        let transfer = Trb([0x5000, 0, 13 << 24 | 3, 7 << 24 | 3 << 16 | 32 << 10 | 1]);
        assert_eq!(
            Event::decode(&transfer),
            Event::Transfer {
                slot: 7,
                endpoint: 3,
                trb: 0x5000,
                code: completion::SHORT_PACKET,
                residue: 3
            }
        );
        let command = Trb([0x6010, 0, 1 << 24, 2 << 24 | 33 << 10]);
        assert_eq!(
            Event::decode(&command),
            Event::Command {
                trb: 0x6010,
                code: 1,
                slot: 2
            }
        );
        assert_eq!(
            Event::decode(&Trb([5 << 24, 0, 1 << 24, 34 << 10])),
            Event::PortStatus { port: 5 }
        );
    }

    #[test]
    fn producer_wraps_through_the_link_and_toggles_cycle() {
        let mut ring = vec![Trb::default(); 4];
        let mut producer = Producer::new(4);
        let link = Trb::link(0x1000);
        let mut indexes = Vec::new();
        for n in 0..5u64 {
            indexes
                .push(producer.push(Trb::normal(n, 1), link, |i, trb| ring[usize::from(i)] = trb));
        }
        assert_eq!(indexes, [0, 1, 2, 0, 1]);
        // The link TRB carries the first pass's cycle; the second pass
        // writes with the toggled cycle.
        assert_eq!(ring[3].kind(), trb_type::LINK);
        assert!(ring[3].cycle());
        assert!(!ring[0].cycle() && ring[0].parameter() == 3);
        assert!(ring[2].cycle() && ring[2].parameter() == 2);
        assert!(!producer.cycle());
    }

    #[test]
    fn consumer_stops_at_the_controllers_position() {
        let mut ring = vec![Trb::default(); 3];
        let mut consumer = Consumer::new(3);
        assert!(consumer.pop(|i| ring[usize::from(i)]).is_none());
        for (i, slot) in ring.iter_mut().enumerate() {
            *slot = Trb::no_op().with_cycle(true);
            slot.0[0] = i as u32;
        }
        for expected in 0..3 {
            assert_eq!(
                consumer.pop(|i| ring[usize::from(i)]).unwrap().0[0],
                expected
            );
        }
        // Wrapped: old cycle-1 entries no longer count.
        assert!(consumer.pop(|i| ring[usize::from(i)]).is_none());
        ring[0] = Trb::no_op().with_cycle(false);
        assert!(consumer.pop(|i| ring[usize::from(i)]).is_some());
        assert_eq!(consumer.index(), 1);
    }

    #[test]
    fn builds_input_contexts() {
        let mut bytes = vec![0xffu8; InputContext::len(32)];
        let mut input = InputContext::new(&mut bytes, 32);
        input.add(0b11);
        input.slot(&Slot {
            speed: Speed::Full,
            root_port: 5,
            route: 0x32,
            last_dci: 1,
            tt: Some((3, 2)),
            multi_tt: true,
            hub: None,
        });
        input.endpoint(1, endpoint_type::CONTROL, 8, 0, 0x8000, true);
        let dword = |bytes: &[u8], context: usize, n: usize| {
            let at = context * 32 + n * 4;
            u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
        };
        assert_eq!(dword(&bytes, 0, 1), 0b11);
        assert_eq!(dword(&bytes, 1, 0), 0x32 | 1 << 20 | 1 << 25 | 1 << 27);
        assert_eq!(dword(&bytes, 1, 1), 5 << 16);
        assert_eq!(dword(&bytes, 1, 2), 3 | 2 << 8);
        assert_eq!(dword(&bytes, 2, 1), 3 << 1 | 4 << 3 | 8 << 16);
        assert_eq!(dword(&bytes, 2, 2), 0x8001);
        assert_eq!(dword(&bytes, 2, 4), 8);
        assert_eq!(dword(&bytes, 3, 0), 0, "the rest is cleared");
        let mut input = InputContext::new(&mut bytes, 32);
        input.endpoint(3, endpoint_type::BULK_IN, 1024, 0, 0x4000, true);
        input.max_burst(3, 3);
        assert_eq!(dword(&bytes, 4, 1), 3 << 1 | 6 << 3 | 3 << 8 | 1024 << 16);
        let mut input = InputContext::new(&mut bytes, 32);
        input.slot(&Slot {
            speed: Speed::High,
            root_port: 1,
            route: 0,
            last_dci: 3,
            tt: None,
            multi_tt: false,
            hub: Some(HubSlot {
                ports: 4,
                think_time: 1,
            }),
        });
        assert_eq!(dword(&bytes, 1, 0), 3 << 20 | 1 << 26 | 3 << 27);
        assert_eq!(dword(&bytes, 1, 1), 1 << 16 | 4 << 24);
        assert_eq!(dword(&bytes, 1, 2), 1 << 16);
    }

    #[test]
    fn port_writes_keep_power_and_never_ack_changes_by_accident() {
        let current = port::POWER | port::CONNECTED | port::ENABLED | port::CONNECT_CHANGE;
        let value = port::write_value(current, port::RESET);
        assert_eq!(value, port::POWER | port::RESET);
        assert_eq!(port::speed(3 << port::SPEED_SHIFT | port::CONNECTED), 3);
    }

    #[test]
    fn converts_intervals() {
        assert_eq!(interrupt_interval(Speed::Full, 10), 6); // 80 µframes
        assert_eq!(interrupt_interval(Speed::Low, 1), 3);
        assert_eq!(interrupt_interval(Speed::Full, 255), 10);
        assert_eq!(interrupt_interval(Speed::High, 4), 3);
        assert_eq!(interrupt_interval(Speed::Super, 0), 0);
    }
}
