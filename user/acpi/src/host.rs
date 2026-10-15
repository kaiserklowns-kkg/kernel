//! The hardware AML reaches (ADR-0153), through the kernel's firmware
//! system calls: I/O ports, the firmware's memory, PCI configuration
//! space (reads), and the embedded controller, spoken to through its two
//! ports.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use oceans_aml::eval::{Host, HostError, Value, space};
use oceans_aml::name::Path;
use oceans_rt::Handle;

const PAGE: u64 = 4096;

/// The embedded controller's ports (ACPI 6.5 §12.2): data, and
/// command/status.
#[derive(Clone, Copy, Debug)]
pub struct Ec {
    pub data: u16,
    pub command: u16,
}

/// Status bits: the output buffer is full (a byte to read), the input
/// buffer is full (the EC has not taken the last byte yet).
const OBF: u64 = 1 << 0;
const IBF: u64 = 1 << 1;
const READ_EC: u64 = 0x80;
const WRITE_EC: u64 = 0x81;
/// Polls of the status before the EC is given up on (each a system call,
/// about a microsecond; after the first few, a millisecond's sleep each).
const EC_POLLS: u32 = 2_000;

pub struct Firmware {
    pub bus: Handle,
    pub ec: Option<Ec>,
    /// The firmware's pages mapped so far, by physical address.
    pages: BTreeMap<u64, *mut u8>,
    /// Objects notified since last looked at.
    pub notified: Vec<(Path, u64)>,
}

impl Firmware {
    pub fn new(bus: Handle) -> Self {
        Self {
            bus,
            ec: None,
            pages: BTreeMap::new(),
            notified: Vec::new(),
        }
    }

    fn port(&self, port: u16, width: u8, write: Option<u64>) -> Result<u64, HostError> {
        oceans_rt::firmware_port(self.bus, port, width, write).map_err(|error| match error {
            oceans_rt::Error::MissingRights => HostError::Denied,
            _ => HostError::Failed,
        })
    }

    /// Waits for the EC's status to have `bit` set (or clear).
    fn ec_wait(&self, ec: Ec, bit: u64, set: bool) -> Result<(), HostError> {
        for poll in 0..EC_POLLS {
            let status = self.port(ec.command, 1, None)?;
            if (status & bit != 0) == set {
                return Ok(());
            }
            if poll > 50 {
                oceans_rt::sleep_ms(1);
            }
        }
        Err(HostError::Failed)
    }

    fn ec_read(&self, address: u8) -> Result<u8, HostError> {
        let ec = self.ec.ok_or(HostError::Failed)?;
        self.ec_wait(ec, IBF, false)?;
        self.port(ec.command, 1, Some(READ_EC))?;
        self.ec_wait(ec, IBF, false)?;
        self.port(ec.data, 1, Some(u64::from(address)))?;
        self.ec_wait(ec, OBF, true)?;
        Ok(self.port(ec.data, 1, None)? as u8)
    }

    fn ec_write(&self, address: u8, value: u8) -> Result<(), HostError> {
        let ec = self.ec.ok_or(HostError::Failed)?;
        self.ec_wait(ec, IBF, false)?;
        self.port(ec.command, 1, Some(WRITE_EC))?;
        self.ec_wait(ec, IBF, false)?;
        self.port(ec.data, 1, Some(u64::from(address)))?;
        self.ec_wait(ec, IBF, false)?;
        self.port(ec.data, 1, Some(u64::from(value)))?;
        self.ec_wait(ec, IBF, false)
    }

    /// The firmware's memory at `address`, `width` bytes, mapped.
    fn memory(&mut self, address: u64, width: u8) -> Result<*mut u8, HostError> {
        let page = address - address % PAGE;
        if (address % PAGE) + u64::from(width) > PAGE {
            return Err(HostError::Failed);
        }
        let base = match self.pages.get(&page) {
            Some(&base) => base,
            None => {
                let (memory, _) = oceans_rt::firmware_memory(self.bus, page, PAGE)
                    .map_err(|_| HostError::Denied)?;
                let prot = oceans_rt::prot::READ | oceans_rt::prot::WRITE;
                let base = oceans_rt::memory_map(memory, 0, prot).map_err(|_| HostError::Failed)?;
                let _ = oceans_rt::close(memory);
                self.pages.insert(page, base);
                base
            }
        };
        // SAFETY: `base` maps the whole page at `page`; the access stays
        // inside it (checked above).
        Ok(unsafe { base.add((address - page) as usize) })
    }
}

impl Host for Firmware {
    fn read(&mut self, space: u8, address: u64, width: u8) -> Result<u64, HostError> {
        match space {
            space::SYSTEM_IO => {
                let port = u16::try_from(address).map_err(|_| HostError::Failed)?;
                self.port(port, width, None)
            }
            space::SYSTEM_MEMORY => {
                let at = self.memory(address, width)?;
                // SAFETY: `at` is mapped for `width` bytes; device and
                // firmware memory is read as the width says, volatile.
                Ok(unsafe {
                    match width {
                        1 => u64::from(at.read_volatile()),
                        2 => u64::from((at as *const u16).read_unaligned()),
                        4 => u64::from((at as *const u32).read_unaligned()),
                        _ => (at as *const u64).read_unaligned(),
                    }
                })
            }
            space::PCI_CONFIG => oceans_rt::firmware_pci_read(self.bus, address, width)
                .map_err(|_| HostError::Failed),
            space::EMBEDDED_CONTROL => {
                let mut value = 0u64;
                for i in 0..u64::from(width) {
                    let at = u8::try_from(address + i).map_err(|_| HostError::Failed)?;
                    value |= u64::from(self.ec_read(at)?) << (8 * i);
                }
                Ok(value)
            }
            _ => Err(HostError::Failed),
        }
    }

    fn write(&mut self, space: u8, address: u64, width: u8, value: u64) -> Result<(), HostError> {
        match space {
            space::SYSTEM_IO => {
                let port = u16::try_from(address).map_err(|_| HostError::Failed)?;
                self.port(port, width, Some(value)).map(drop)
            }
            space::SYSTEM_MEMORY => {
                let at = self.memory(address, width)?;
                // SAFETY: as for reading.
                unsafe {
                    match width {
                        1 => at.write_volatile(value as u8),
                        2 => (at as *mut u16).write_unaligned(value as u16),
                        4 => (at as *mut u32).write_unaligned(value as u32),
                        _ => (at as *mut u64).write_unaligned(value),
                    }
                }
                Ok(())
            }
            space::EMBEDDED_CONTROL => {
                for i in 0..u64::from(width) {
                    let at = u8::try_from(address + i).map_err(|_| HostError::Failed)?;
                    self.ec_write(at, (value >> (8 * i)) as u8)?;
                }
                Ok(())
            }
            // PCI configuration is read only: a device's registers stay
            // its driver's.
            _ => Err(HostError::Denied),
        }
    }

    fn sleep(&mut self, ms: u64) {
        oceans_rt::sleep_ms(ms);
    }

    fn stall(&mut self, us: u64) {
        // Under a millisecond: a busy wait is what `Stall` asks for, and
        // it is bounded (the interpreter caps it at 100 µs).
        let until = oceans_rt::clock_ms() + us.div_ceil(1000);
        while oceans_rt::clock_ms() < until {
            core::hint::spin_loop();
        }
    }

    fn timer(&mut self) -> u64 {
        oceans_rt::clock_ms().saturating_mul(10_000)
    }

    fn notify(&mut self, object: &Path, value: u64) {
        if self.notified.len() < 64 {
            self.notified.push((object.clone(), value));
        }
    }

    fn debug(&mut self, _value: &Value) {}
}

/// The EC's ports from its `_CRS` (a resource template): the first two
/// I/O port descriptors are data and command/status.
pub fn ec_ports(resources: &[u8]) -> Option<Ec> {
    let mut ports = Vec::new();
    let mut at = 0;
    while at < resources.len() && ports.len() < 2 {
        let tag = resources[at];
        if tag & 0x80 != 0 {
            // A large item: its 16-bit length follows.
            let len =
                usize::from(*resources.get(at + 1)?) | usize::from(*resources.get(at + 2)?) << 8;
            at += 3 + len;
            continue;
        }
        let (kind, len) = ((tag >> 3) & 0x0f, usize::from(tag & 0x07));
        match kind {
            // I/O port: info, minimum, maximum, alignment, length.
            0x08 => {
                let low = *resources.get(at + 2)?;
                let high = *resources.get(at + 3)?;
                ports.push(u16::from_le_bytes([low, high]));
            }
            // Fixed I/O port: base, length.
            0x09 => {
                let low = *resources.get(at + 1)?;
                let high = *resources.get(at + 2)?;
                ports.push(u16::from_le_bytes([low, high]) & 0x3ff);
            }
            0x0f => break,
            _ => {}
        }
        at += 1 + len;
    }
    match ports[..] {
        [data, command] => Some(Ec { data, command }),
        _ => None,
    }
}
