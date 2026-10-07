//! What switching off and restarting need from the CPU and the chipset
//! (ADR-0085): I/O port access of a given width, a delay that works with
//! interrupts disabled, and the legacy ways to restart a PC.

use core::arch::asm;

use ::x86_64::instructions::port::Port;

/// Reads `bits` (8, 16 or 32) from I/O port `port`.
pub fn io_read(port: u16, bits: u8) -> u32 {
    // SAFETY: only called with the firmware's ACPI power registers, which
    // reading has no side effect on.
    unsafe {
        match bits {
            8 => u32::from(Port::<u8>::new(port).read()),
            16 => u32::from(Port::<u16>::new(port).read()),
            _ => Port::<u32>::new(port).read(),
        }
    }
}

/// Writes the low `bits` (8, 16 or 32) of `value` to I/O port `port`.
pub fn io_write(port: u16, bits: u8, value: u32) {
    // SAFETY: only called to switch off or restart the machine, with the
    // registers and values the firmware described for exactly that.
    unsafe {
        match bits {
            8 => Port::<u8>::new(port).write(value as u8),
            16 => Port::<u16>::new(port).write(value as u16),
            _ => Port::<u32>::new(port).write(value),
        }
    }
}

/// Waits about `ms` milliseconds without interrupts: each write to the
/// POST diagnostic port 0x80 takes about a microsecond on a PC.
pub fn delay_ms(ms: u64) {
    let mut port = Port::<u8>::new(0x80);
    for _ in 0..ms * 1000 {
        // SAFETY: port 0x80 is the POST code port, written by firmware for
        // diagnostics and by kernels for exactly this kind of delay.
        unsafe { port.write(0) };
    }
}

/// Writes back and invalidates the CPU caches, so nothing is left only in
/// them when the power goes (the ACPI sleep sequence asks for it).
pub fn flush_caches() {
    // SAFETY: WBINVD only writes dirty lines back; the kernel runs at CPL 0.
    unsafe { asm!("wbinvd", options(nostack, preserves_flags)) };
}

/// Writes a byte to PCI configuration space of bus 0 through the legacy
/// mechanism (ports 0xCF8/0xCFC): an ACPI reset register may live there.
pub fn pci_config_write_u8(device: u8, function: u8, offset: u16, value: u8) {
    let address = 0x8000_0000u32
        | (u32::from(device & 0x1f) << 11)
        | (u32::from(function & 0x7) << 8)
        | (u32::from(offset) & 0xfc);
    // SAFETY: the address names the register the firmware gave for resets.
    unsafe {
        Port::<u32>::new(0xcf8).write(address);
        Port::<u8>::new(0xcfc + (offset & 3)).write(value);
    }
}

/// The PCI reset control register (0xCF9) of Intel and AMD chipsets: a
/// full reset, as Linux does it.
pub fn reset_through_cf9() {
    let mut port = Port::<u8>::new(0xcf9);
    // SAFETY: 0xCF9 is the chipset's reset control; bit 1 asks for a hard
    // reset, bit 2 starts it.
    unsafe {
        let value = port.read() & !0b110;
        port.write(value | 0b010);
        delay_ms(1);
        port.write(value | 0b110);
    }
}

/// Pulses the CPU reset line through the 8042 keyboard controller.
pub fn reset_through_keyboard_controller() {
    let mut status = Port::<u8>::new(0x64);
    // SAFETY: reading the 8042 status and sending it command 0xFE (pulse
    // reset) has no other effect; on a machine without one the port is
    // unclaimed and reads 0xFF.
    unsafe {
        // Its input buffer empty first (bit 1), for at most ~10 ms.
        for _ in 0..10 {
            if status.read() & 0b10 == 0 {
                break;
            }
            delay_ms(1);
        }
        status.write(0xfe);
    }
}

/// Restarts the CPU by a triple fault: an empty interrupt table, then an
/// exception. Every PC resets on it.
pub fn triple_fault() -> ! {
    let pointer = ::x86_64::structures::DescriptorTablePointer {
        limit: 0,
        base: ::x86_64::VirtAddr::zero(),
    };
    // SAFETY: deliberately makes every interrupt unhandleable; nothing runs
    // afterwards.
    unsafe {
        ::x86_64::instructions::tables::lidt(&pointer);
        asm!("int3", options(noreturn));
    }
}
