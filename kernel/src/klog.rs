//! Kernel logging.
//!
//! Lines have the form `[LEVEL] subsystem: message` and go to the
//! architecture console (serial on x86_64). Diagnostics never depend on a
//! display (master spec §42).

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
            } else {
                crate::display::write(s.as_bytes());
            }
        }
        Ok(())
    }
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
pub(crate) use {debug, error, info, log_at, warn_ as warn};
