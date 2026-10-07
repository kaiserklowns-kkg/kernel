//! The system console (ADR-0017): raw bytes in from the serial line, raw
//! bytes out, for userspace holding a console capability.
//!
//! The kernel adds no policy: no echo, no line editing, no translation.
//! Whoever reads the console (the shell, a terminal service) decides. Input
//! is buffered in a fixed ring (no allocation in interrupt context); when it
//! is full, further bytes are dropped and counted.
//!
//! Keyboard input (the PS/2 keyboard, and USB keyboards through
//! `CONSOLE_INPUT`) can be taken by the desktop (ADR-0059): it then queues
//! for the desktop, which hands it to the focused window (the Terminal
//! puts it back here). Serial input always comes here.

use alloc::collections::VecDeque;
use alloc::sync::Arc;

use spin::Mutex;

use crate::acpi::Acpi;
use crate::ipc::Notification;
use crate::process::Process;
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

const KEYS_CAPACITY: usize = oceans_abi::display::MAX_KEYS;

/// Keyboard input taken by the desktop (ADR-0059). Always locked with
/// interrupts disabled: the PS/2 interrupt feeds it.
struct Keys {
    ring: [u8; KEYS_CAPACITY],
    head: usize,
    len: usize,
    /// The holder (by address: compared, never dereferenced), and what to
    /// signal when keys arrive.
    holder: Option<(usize, Arc<Notification>, u64)>,
}

static KEYS: Mutex<Keys> = Mutex::new(Keys {
    ring: [0; KEYS_CAPACITY],
    head: 0,
    len: 0,
    holder: None,
});

fn identity(process: &Process) -> usize {
    core::ptr::from_ref(process) as usize
}

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
        on_key,
    );
    match result {
        Ok(()) => klog::info!("console input: PS/2 keyboard (IRQ {KEYBOARD_IRQ}, GSI {gsi})"),
        Err(problem) => klog::info!("no keyboard input: {problem}"),
    }
}

/// The serial line's diagnostic key, Ctrl+\\ (0x1C): the kernel logs what
/// every CPU runs and every process's thread waits on, instead of passing
/// the byte on (ADR-0089).
const DIAGNOSTIC_KEY: u8 = 0x1c;

/// A received byte (interrupt context, interrupts disabled).
fn on_input(byte: u8) {
    crate::random::sample();
    if byte == DIAGNOSTIC_KEY {
        crate::sched::dump();
        crate::process::dump();
        return;
    }
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

/// A key from a keyboard (interrupt context, or `inject`; interrupts
/// disabled): to the desktop if it took the keyboard, else to the console.
fn on_key(byte: u8) {
    {
        let mut keys = KEYS.lock();
        let keys = &mut *keys;
        if let Some((_, notification, bits)) = &keys.holder {
            crate::random::sample();
            if keys.len < KEYS_CAPACITY {
                keys.ring[(keys.head + keys.len) % KEYS_CAPACITY] = byte;
                keys.len += 1;
            }
            notification.signal(*bits);
            return;
        }
    }
    on_input(byte);
}

/// Input from a keyboard driver in user space (ADR-0032), as if typed;
/// from the keyboard's holder (the desktop handing keys to the Terminal,
/// ADR-0059), straight to the console.
pub fn inject(from: &Process, bytes: &[u8]) {
    arch::without_interrupts(|| {
        let holder = KEYS
            .lock()
            .holder
            .as_ref()
            .is_some_and(|(who, _, _)| *who == identity(from));
        for &byte in bytes {
            if holder {
                on_input(byte);
            } else {
                on_key(byte);
            }
        }
    });
}

/// Keyboard input goes to `holder` from now on (`DISPLAY_KEYBOARD`):
/// queued, with `bits` signalled on `notification`.
pub fn take_keyboard(holder: &Process, notification: Arc<Notification>, bits: u64) {
    let old = arch::without_interrupts(|| {
        let mut keys = KEYS.lock();
        keys.head = 0;
        keys.len = 0;
        keys.holder.replace((identity(holder), notification, bits))
    });
    // Dropped with interrupts enabled, outside the lock.
    drop(old);
}

/// Takes queued keys for `holder` (`DISPLAY_KEYS`); `None` if it does not
/// hold the keyboard.
pub fn read_keys(holder: &Process, out: &mut [u8]) -> Option<usize> {
    arch::without_interrupts(|| {
        let mut keys = KEYS.lock();
        if !keys
            .holder
            .as_ref()
            .is_some_and(|(who, _, _)| *who == identity(holder))
        {
            return None;
        }
        let count = out.len().min(keys.len);
        for slot in out.iter_mut().take(count) {
            *slot = keys.ring[keys.head];
            keys.head = (keys.head + 1) % KEYS_CAPACITY;
            keys.len -= 1;
        }
        Some(count)
    })
}

/// `process` exited: if it held the keyboard, keys go to the console again
/// (the queued ones are dropped).
pub fn process_exited(process: &Process) {
    let old = arch::without_interrupts(|| {
        let mut keys = KEYS.lock();
        if !keys
            .holder
            .as_ref()
            .is_some_and(|(who, _, _)| *who == identity(process))
        {
            return None;
        }
        keys.len = 0;
        keys.holder.take()
    });
    drop(old);
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
                // Interrupted (ADR-0044): nothing read; the caller exits.
                if sched::interrupted() {
                    let me = sched::current();
                    input.readers.retain(|reader| !Arc::ptr_eq(reader, &me));
                    return 0;
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
