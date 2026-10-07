//! Kernel logging.
//!
//! Lines have the form `[LEVEL] subsystem: message` and go to the
//! architecture console (serial on x86_64). Diagnostics never depend on a
//! display (master spec §42). Lines at INFO and above are also kept in a
//! ring for `LOG_READ` (ADR-0070), so diagnostics need no serial cable.

use core::fmt::{self, Write};

use spin::Mutex;

use crate::{arch, sched};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    const fn tag(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN ",
            Self::Info => "INFO ",
            Self::Debug => "DEBUG",
        }
    }
}

/// Most verbose level that is emitted. Runtime filtering comes later.
const MAX_LEVEL: Level = Level::Debug;

/// Serialises whole lines so concurrent writers never interleave.
///
/// Held with **preemption** disabled but interrupts enabled: serial output
/// is slow, and the UART receive interrupt must keep draining input while
/// it runs (ADR-0018). No interrupt handler takes this lock, and holders
/// never block, so this cannot deadlock.
static CONSOLE: Mutex<()> = Mutex::new(());

/// Writes one log line, waiting for the console if necessary.
pub fn write(level: Level, target: &str, args: fmt::Arguments<'_>) {
    if level > MAX_LEVEL {
        return;
    }
    let _no_preempt = sched::NoPreempt::new();
    let _guard = CONSOLE.lock();
    emit(level, target, args, false);
}

/// Writes raw bytes (user console output) as one unit, never inside a log
/// line.
pub fn write_raw(bytes: &[u8]) {
    let _no_preempt = sched::NoPreempt::new();
    let _guard = CONSOLE.lock();
    arch::console_write_bytes(bytes);
    crate::display::write(bytes);
}

/// Writes one log line without ever blocking.
///
/// For panic and fatal-exception paths, where the interrupted code may hold
/// the console lock. Output may interleave with that writer; losing the
/// diagnostic would be worse.
pub fn emergency(level: Level, target: &str, args: fmt::Arguments<'_>) {
    arch::without_interrupts(|| {
        let _guard = CONSOLE.try_lock();
        emit(level, target, args, true);
    });
}

fn emit(level: Level, target: &str, args: fmt::Arguments<'_>, emergency: bool) {
    let target = match target.strip_prefix("oceans_kernel") {
        Some("") => "kernel",
        Some(rest) => rest.trim_start_matches("::"),
        None => target,
    };
    // Console writes are infallible; a formatting error inside `args` only
    // truncates this line.
    let mut console = Console {
        screen: level <= SCREEN_LEVEL,
        emergency,
    };
    let _ = writeln!(console, "[{}] {}: {}", level.tag(), target, args);
}

/// Most verbose level drawn on the screen: debug lines go to serial only.
const SCREEN_LEVEL: Level = Level::Info;

struct Console {
    screen: bool,
    /// Never wait for the display (a panic may have interrupted it).
    emergency: bool,
}

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        arch::console_write(s);
        if self.screen {
            if self.emergency {
                crate::display::try_write(s.as_bytes());
                if let Some(mut ring) = RING.try_lock() {
                    ring.append(s.as_bytes());
                }
            } else {
                crate::display::write(s.as_bytes());
                RING.lock().append(s.as_bytes());
            }
        }
        Ok(())
    }
}

const RING_SIZE: usize = oceans_abi::LOG_RING;

/// The log kept for `LOG_READ` (ADR-0070): the last `RING_SIZE` bytes of
/// lines at INFO and above. Taken with preemption disabled (under
/// `CONSOLE`, or by `read`); never by an interrupt handler.
struct Ring {
    bytes: [u8; RING_SIZE],
    /// Bytes ever written: the next byte's position.
    written: u64,
}

static RING: Mutex<Ring> = Mutex::new(Ring {
    bytes: [0; RING_SIZE],
    written: 0,
});

impl Ring {
    fn append(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.bytes[(self.written % RING_SIZE as u64) as usize] = byte;
            self.written += 1;
        }
    }

    /// Copies from position `from` (or the oldest byte kept) into `out`;
    /// returns how many and from where.
    fn read(&self, from: u64, out: &mut [u8]) -> (usize, u64) {
        let oldest = self.written.saturating_sub(RING_SIZE as u64);
        let start = from.clamp(oldest, self.written);
        let count = ((self.written - start) as usize).min(out.len());
        for (i, slot) in out[..count].iter_mut().enumerate() {
            *slot = self.bytes[((start + i as u64) % RING_SIZE as u64) as usize];
        }
        (count, start)
    }
}

/// `LOG_READ`: kept log text from position `from` (see [`Ring::read`]).
pub fn read(from: u64, out: &mut [u8]) -> (usize, u64) {
    let _no_preempt = sched::NoPreempt::new();
    RING.lock().read(from, out)
}

/// A diagnostic line from interrupt context (the diagnostic key,
/// ADR-0089): through [`emergency`], so it never waits for the console.
macro_rules! diagnostic {
    ($($arg:tt)+) => {
        $crate::klog::emergency($crate::klog::Level::Info, "diag", format_args!($($arg)+))
    };
}

macro_rules! log_at {
    ($level:ident, $($arg:tt)+) => {
        $crate::klog::write($crate::klog::Level::$level, module_path!(), format_args!($($arg)+))
    };
}

#[allow(unused_macros)] // no error-level call sites yet
macro_rules! error { ($($arg:tt)+) => { $crate::klog::log_at!(Error, $($arg)+) }; }
macro_rules! warn_ { ($($arg:tt)+) => { $crate::klog::log_at!(Warn, $($arg)+) }; }
macro_rules! info { ($($arg:tt)+) => { $crate::klog::log_at!(Info, $($arg)+) }; }
macro_rules! debug { ($($arg:tt)+) => { $crate::klog::log_at!(Debug, $($arg)+) }; }

// `warn` collides with the built-in lint attribute, so it is defined under a
// different name and re-exported.
#[allow(unused_imports)]
pub(crate) use {debug, diagnostic, error, info, log_at, warn_ as warn};
