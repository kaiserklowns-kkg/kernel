//! I/O APIC: routes device interrupt lines (global system interrupts) to
//! CPU vectors (ADR-0017). Every line is masked at init; only explicitly
//! routed ones are delivered.

use crate::klog;
use crate::memory::paging;

const IOREGSEL: usize = 0x00;
const IOWIN: usize = 0x10;
const REG_VERSION: u32 = 0x01;
const REG_REDIRECTION_BASE: u32 = 0x10;

const MASKED: u32 = 1 << 16;
const LEVEL_TRIGGERED: u32 = 1 << 15;
const ACTIVE_LOW: u32 = 1 << 13;

pub struct IoApic {
    base: *mut u32,
    gsi_base: u32,
    entries: u32,
}

// SAFETY: the registers are only accessed with interrupts disabled, from
// behind the mutex that holds the I/O APIC (`arch::io_apic`).
unsafe impl Send for IoApic {}

impl IoApic {
    /// Maps the I/O APIC at physical `address` and masks every line.
    pub fn new(address: u64, gsi_base: u32) -> Result<Self, paging::MapError> {
        let base = paging::map_mmio(address, 0x20)?.cast::<u32>();
        let mut io = Self {
            base,
            gsi_base,
            entries: 0,
        };
        io.entries = ((io.read(REG_VERSION) >> 16) & 0xff) + 1;
        for line in 0..io.entries {
            io.write_entry(line, u64::from(MASKED));
        }
        klog::debug!(
            "I/O APIC at {address:#x}: GSIs {gsi_base}..{}",
            gsi_base + io.entries
        );
        Ok(io)
    }

    fn read(&mut self, register: u32) -> u32 {
        // SAFETY: `base` maps the I/O APIC's register window uncached;
        // IOREGSEL/IOWIN are the architected indirect access pair.
        unsafe {
            self.base.byte_add(IOREGSEL).write_volatile(register);
            self.base.byte_add(IOWIN).read_volatile()
        }
    }

    fn write(&mut self, register: u32, value: u32) {
        // SAFETY: as for `read`.
        unsafe {
            self.base.byte_add(IOREGSEL).write_volatile(register);
            self.base.byte_add(IOWIN).write_volatile(value);
        }
    }

    fn write_entry(&mut self, line: u32, entry: u64) {
        let register = REG_REDIRECTION_BASE + 2 * line;
        // High half (destination) first, then the low half that unmasks.
        self.write(register + 1, (entry >> 32) as u32);
        self.write(register, entry as u32);
    }

    /// Delivers `gsi` as `vector` to the local APIC `destination`.
    pub fn route(
        &mut self,
        gsi: u32,
        vector: u8,
        active_low: bool,
        level_triggered: bool,
        destination: u8,
    ) -> bool {
        let Some(line) = gsi.checked_sub(self.gsi_base).filter(|&l| l < self.entries) else {
            return false;
        };
        let mut low = u32::from(vector);
        if active_low {
            low |= ACTIVE_LOW;
        }
        if level_triggered {
            low |= LEVEL_TRIGGERED;
        }
        self.write_entry(line, (u64::from(destination) << 56) | u64::from(low));
        true
    }
}
