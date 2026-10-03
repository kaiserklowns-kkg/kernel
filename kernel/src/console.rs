//! The system console (ADR-0017): raw bytes in from the serial line, raw
//! bytes out, for userspace holding a console capability.
//!
//! The kernel adds no policy: no echo, no line editing, no translation.
//! Whoever reads the console (the shell, a terminal service) decides. Input
//! is buffered in a fixed ring (no allocation in interrupt context); when it
//! is full, further bytes are dropped and counted.

use alloc::collections::VecDeque;
use alloc::sync::Arc;

use spin::Mutex;

use crate::acpi::Acpi;
use crate::sched::{self, Thread};
use crate::{arch, klog};

const CAPACITY: usize = 4096;
/// COM1's ISA interrupt line.
const COM1_IRQ: u8 = 4;
/// The PS/2 keyboard's ISA interrupt line.
const KEYBOARD_IRQ: u8 = 1;

struct Input {
    ring: [u8; CAPACITY],
    head: usize,
    len: usize,
    dropped: u64,
    readers: VecDeque<Arc<Thread>>,
}

static INPUT: Mutex<Input> = Mutex::new(Input {
    ring: [0; CAPACITY],
    head: 0,
    len: 0,
    dropped: 0,
    readers: VecDeque::new(),
});

/// Routes the serial line's interrupt and starts accepting input.
pub fn init(acpi: Option<&Acpi>) {
    let Some(acpi) = acpi else {
        klog::warn!("console input disabled: no ACPI interrupt routing information");
        return;
    };
    let Some((gsi, flags)) = acpi.isa_irq(COM1_IRQ) else {
        klog::warn!("console input disabled: no ACPI interrupt routing information");
        return;
    };
    let Some(io_apic) = acpi.io_apic_for(gsi) else {
        klog::warn!("console input disabled: no I/O APIC serves GSI {gsi}");
        return;
    };
    let result = arch::enable_console_input(
        io_apic.address,
        io_apic.gsi_base,
        gsi,
        oceans_acpi::active_low(flags),
        oceans_acpi::level_triggered(flags),
        on_input,
    );
    match result {
        Ok(()) => klog::info!("console input: COM1 (IRQ {COM1_IRQ}, GSI {gsi}) via I/O APIC"),
        Err(problem) => klog::warn!("console input disabled: {problem}"),
    }

    // The PS/2 keyboard feeds the same input (ADR-0029).
    let Some((gsi, flags)) = acpi.isa_irq(KEYBOARD_IRQ) else {
        return;
    };
    let Some(io_apic) = acpi.io_apic_for(gsi) else {
        return;
    };
    let result = arch::enable_keyboard(
        io_apic.address,
        io_apic.gsi_base,
        gsi,
        oceans_acpi::active_low(flags),
        oceans_acpi::level_triggered(flags),
        on_input,
    );
    match result {
        Ok(()) => klog::info!("console input: PS/2 keyboard (IRQ {KEYBOARD_IRQ}, GSI {gsi})"),
        Err(problem) => klog::info!("no keyboard input: {problem}"),
    }
}

/// A received byte (interrupt context, interrupts disabled).
fn on_input(byte: u8) {
    crate::random::sample();
    let reader = {
        let mut input = INPUT.lock();
        if input.len == CAPACITY {
            input.dropped += 1;
        } else {
            let tail = (input.head + input.len) % CAPACITY;
            input.ring[tail] = byte;
            input.len += 1;
        }
        input.readers.pop_front()
    };
    if let Some(reader) = reader {
        sched::wake(reader);
    }
}

/// Input from a keyboard driver in user space (ADR-0032), as if typed.
pub fn inject(bytes: &[u8]) {
    arch::without_interrupts(|| {
        for &byte in bytes {
            on_input(byte);
        }
    });
}

/// Blocks until input is available; copies up to `out.len()` bytes and
/// returns how many. `out` must not be empty.
pub fn read(out: &mut [u8]) -> usize {
    debug_assert!(!out.is_empty());
    arch::without_interrupts(|| {
        loop {
            {
                let mut input = INPUT.lock();
                if input.len > 0 {
                    let count = out.len().min(input.len);
                    for slot in out.iter_mut().take(count) {
                        *slot = input.ring[input.head];
                        input.head = (input.head + 1) % CAPACITY;
                        input.len -= 1;
                    }
                    if input.dropped > 0 {
                        let dropped = core::mem::take(&mut input.dropped);
                        drop(input);
                        klog::warn!("console input overflow: {dropped} bytes dropped");
                    }
                    return count;
                }
                input.readers.push_back(sched::current());
            }
            sched::block();
        }
    })
}

/// Writes raw bytes to the console, never interleaved inside a kernel log
/// line.
pub fn write(bytes: &[u8]) {
    klog::write_raw(bytes);
}
