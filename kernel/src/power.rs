//! Switching the machine off and restarting it (ADR-0085).
//!
//! What to write where comes from the firmware: the FADT's PM1 control
//! registers and reset register, and the `\_S5` sleep types of the DSDT
//! (`oceans-acpi`). Only init may ask (`SYSTEM_POWER` needs `MANAGE` on
//! the system information object), after it has stopped the services and
//! synced the disks.
//!
//! - **Off:** ACPI S5. The firmware is switched to ACPI mode if it is not
//!   (`SMI_CMD`), the caches are flushed, then SLP_TYP and SLP_EN go to
//!   PM1a (and PM1b) control, or to the sleep control register on
//!   hardware-reduced ACPI. Without `\_S5` Oceans cannot switch this
//!   machine off, and says so before anything stops.
//! - **Restart:** the ACPI reset register when the firmware offers it,
//!   then the chipset's reset control (0xCF9), the keyboard controller and
//!   a triple fault, each after the previous had time to work.
//!
//! No AML is run: `\_PTS` and `\_TTS` are skipped, as Oceans has no AML
//! interpreter. PCs switch off without them.

use oceans_abi::power;
use oceans_acpi::{Fadt, GenericAddress, space};

use crate::arch::{self, power as hw};
use crate::{klog, memory};

/// PM1 control bits.
const SCI_EN: u32 = 1 << 0;
const SLP_TYP_SHIFT: u32 = 10;
const SLP_TYP_MASK: u32 = 0b111 << SLP_TYP_SHIFT;
const SLP_EN: u32 = 1 << 13;
/// Hardware-reduced sleep control bits.
const HW_SLP_TYP_SHIFT: u32 = 2;
const HW_SLP_EN: u32 = 1 << 5;
/// How long each attempt gets before the next.
const OFF_WAIT_MS: u64 = 3000;
const RESTART_WAIT_MS: u64 = 500;
const ACPI_ENABLE_WAIT_MS: u64 = 3000;

/// A register the firmware described, ready to use: an I/O port, or
/// memory mapped once at boot (MMIO mappings are permanent).
#[derive(Clone, Copy, Debug)]
struct Register {
    gas: GenericAddress,
    /// The mapping of a memory register (0 for a port).
    virt: usize,
}

impl Register {
    fn new(gas: GenericAddress) -> Option<Self> {
        match gas.space {
            space::SYSTEM_IO if gas.address <= u64::from(u16::MAX) => Some(Self { gas, virt: 0 }),
            space::SYSTEM_MEMORY => memory::paging::map_mmio(gas.address, 4)
                .ok()
                .map(|pointer| Self {
                    gas,
                    virt: pointer as usize,
                }),
            _ => None,
        }
    }

    /// Its width: what the firmware says, 16 bits (PM1's) if unclear.
    fn width(&self) -> u8 {
        match self.gas.bit_width {
            8 | 16 | 32 => self.gas.bit_width,
            _ => 16,
        }
    }

    fn read(&self) -> u32 {
        let bits = self.width();
        if self.virt == 0 {
            return hw::io_read(self.gas.address as u16, bits);
        }
        let pointer = self.virt as *const u8;
        // SAFETY: the firmware's register, mapped uncached at boot.
        unsafe {
            match bits {
                8 => u32::from(pointer.read_volatile()),
                16 => u32::from(pointer.cast::<u16>().read_volatile()),
                _ => pointer.cast::<u32>().read_volatile(),
            }
        }
    }

    fn write(&self, value: u32) {
        self.write_width(self.width(), value);
    }

    fn write_width(&self, bits: u8, value: u32) {
        if self.virt == 0 {
            return hw::io_write(self.gas.address as u16, bits, value);
        }
        let pointer = self.virt as *mut u8;
        // SAFETY: the firmware's register, mapped uncached at boot.
        unsafe {
            match bits {
                8 => pointer.write_volatile(value as u8),
                16 => pointer.cast::<u16>().write_volatile(value as u16),
                _ => pointer.cast::<u32>().write_volatile(value),
            }
        }
    }
}

/// The reset register: a [`Register`], or a byte of PCI configuration
/// space on bus 0.
#[derive(Clone, Copy, Debug)]
enum Reset {
    Register(Register),
    Pci {
        device: u8,
        function: u8,
        offset: u16,
    },
}

impl Reset {
    fn new(gas: GenericAddress) -> Option<Self> {
        if gas.space != space::PCI_CONFIG {
            return Register::new(gas).map(Self::Register);
        }
        // Bus 0: device in bits 32-47, function in 16-31, offset in 0-15.
        (gas.address >> 48 == 0).then_some(Self::Pci {
            device: (gas.address >> 32) as u8,
            function: (gas.address >> 16) as u8,
            offset: gas.address as u16,
        })
    }
}

/// How this machine is switched off.
#[derive(Clone, Copy, Debug)]
enum Off {
    Pm1 {
        a: Register,
        b: Option<Register>,
        types: (u8, u8),
        smi_command: u32,
        acpi_enable: u8,
    },
    Reduced {
        control: Register,
        sleep_type: u8,
    },
}

#[derive(Clone, Copy, Debug)]
struct Power {
    off: Option<Off>,
    reset: Option<(Reset, u8)>,
}

static POWER: spin::Once<Power> = spin::Once::new();

/// Records what the firmware said (`types`: the `\_S5` sleep types, if
/// found), and logs it. Without a FADT the machine can still restart.
pub fn init(fadt: Option<&Fadt>, types: Option<(u8, u8)>) {
    let off = match (fadt, types) {
        (Some(fadt), Some((a, _))) if fadt.hardware_reduced => fadt
            .sleep_control
            .and_then(Register::new)
            .map(|control| Off::Reduced {
                control,
                sleep_type: a,
            }),
        (Some(fadt), Some(types)) => fadt.pm1a_control.and_then(Register::new).map(|a| Off::Pm1 {
            a,
            b: fadt.pm1b_control.and_then(Register::new),
            types,
            smi_command: fadt.smi_command,
            acpi_enable: fadt.acpi_enable,
        }),
        _ => None,
    };
    let reset_register = fadt
        .and_then(|fadt| fadt.reset)
        .and_then(|(gas, value)| Some((gas, Reset::new(gas)?, value)));

    match (&off, fadt, types) {
        (Some(Off::Pm1 { a, types, .. }), _, _) => klog::info!(
            "ACPI: switching off through PM1 control at {} {:#x} (SLP_TYP {}/{})",
            space_name(a.gas.space),
            a.gas.address,
            types.0,
            types.1
        ),
        (
            Some(Off::Reduced {
                control,
                sleep_type,
            }),
            _,
            _,
        ) => klog::info!(
            "ACPI: switching off through the sleep control register at {} {:#x} (SLP_TYP {sleep_type})",
            space_name(control.gas.space),
            control.gas.address
        ),
        (None, None, _) => klog::warn!("ACPI: no FADT; Oceans cannot switch this machine off"),
        (None, _, None) => {
            klog::warn!("ACPI: no \\_S5 sleep type; Oceans cannot switch this machine off")
        }
        (None, _, _) => klog::warn!(
            "ACPI: the FADT has no usable sleep control; Oceans cannot switch this machine off"
        ),
    }
    match &reset_register {
        Some((gas, _, value)) => klog::info!(
            "ACPI: restarting through the reset register at {} {:#x} (value {value:#x})",
            space_name(gas.space),
            gas.address
        ),
        None => klog::info!("ACPI: no reset register; restarting through the chipset"),
    }
    let reset = reset_register.map(|(_, reset, value)| (reset, value));
    POWER.call_once(|| Power { off, reset });
}

fn space_name(space: u8) -> &'static str {
    match space {
        space::SYSTEM_IO => "io",
        space::SYSTEM_MEMORY => "memory",
        space::PCI_CONFIG => "pci",
        _ => "?",
    }
}

/// `power::CAN_*` bits for this machine. Restarting always works: a
/// triple fault resets any PC.
pub fn capabilities() -> u64 {
    let off = POWER.get().is_some_and(|p| p.off.is_some());
    power::CAN_RESTART | if off { power::CAN_OFF } else { 0 }
}

/// Switches the machine off. Returns only if this machine cannot be
/// switched off, and then nothing was done.
pub fn off() {
    let Some(off) = POWER.get().and_then(|p| p.off) else {
        return;
    };
    klog::info!("switching off (ACPI S5)");
    arch::console_drain();
    arch::disable_interrupts();
    match off {
        Off::Pm1 {
            a,
            b,
            types,
            smi_command,
            acpi_enable,
        } => {
            enable_acpi_mode(&a, smi_command, acpi_enable);
            let prepared = |register: &Register, sleep_type: u8| {
                (register.read() & !(SLP_TYP_MASK | SLP_EN))
                    | (u32::from(sleep_type) << SLP_TYP_SHIFT)
            };
            let value_a = prepared(&a, types.0);
            let value_b = b.as_ref().map(|b| (b, prepared(b, types.1)));
            // The type first, then the enable bit, as ACPI asks.
            a.write(value_a);
            if let Some((b, value)) = value_b {
                b.write(value);
            }
            hw::flush_caches();
            a.write(value_a | SLP_EN);
            if let Some((b, value)) = value_b {
                b.write(value | SLP_EN);
            }
        }
        Off::Reduced {
            control,
            sleep_type,
        } => {
            hw::flush_caches();
            control.write_width(8, (u32::from(sleep_type) << HW_SLP_TYP_SHIFT) | HW_SLP_EN);
        }
    }
    hw::delay_ms(OFF_WAIT_MS);
    klog::error!("the machine did not switch off; it is safe to turn it off now");
    arch::halt_forever()
}

/// Restarts the machine.
pub fn restart() -> ! {
    klog::info!("restarting");
    arch::console_drain();
    arch::disable_interrupts();
    hw::flush_caches();
    if let Some((reset, value)) = POWER.get().and_then(|p| p.reset) {
        match reset {
            Reset::Register(register) => register.write_width(8, u32::from(value)),
            Reset::Pci {
                device,
                function,
                offset,
            } => hw::pci_config_write_u8(device, function, offset, value),
        }
        hw::delay_ms(RESTART_WAIT_MS);
        klog::warn!("the reset register did not restart the machine");
    }
    hw::reset_through_cf9();
    hw::delay_ms(RESTART_WAIT_MS);
    klog::warn!("the chipset did not restart the machine; trying the keyboard controller");
    hw::reset_through_keyboard_controller();
    hw::delay_ms(RESTART_WAIT_MS);
    klog::warn!("forcing a restart (triple fault)");
    hw::triple_fault()
}

/// If the firmware still handles power events itself (SCI_EN clear), asks
/// it to hand them to the OS, as the sleep registers need.
fn enable_acpi_mode(pm1a: &Register, smi_command: u32, acpi_enable: u8) {
    if pm1a.read() & SCI_EN != 0 || smi_command == 0 || acpi_enable == 0 {
        return;
    }
    let Ok(port) = u16::try_from(smi_command) else {
        return;
    };
    hw::io_write(port, 8, u32::from(acpi_enable));
    for _ in 0..ACPI_ENABLE_WAIT_MS {
        if pm1a.read() & SCI_EN != 0 {
            return;
        }
        hw::delay_ms(1);
    }
    klog::warn!("the firmware did not enter ACPI mode");
}

/// Smoke test: QEMU's q35 firmware describes both ways.
pub fn self_test() {
    assert_eq!(
        capabilities(),
        power::CAN_OFF | power::CAN_RESTART,
        "QEMU's firmware describes switching off"
    );
    klog::info!("self-test passed");
}
