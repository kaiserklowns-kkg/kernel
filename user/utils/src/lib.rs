//! Shared start-up for the utilities: find the console output and the
//! capabilities they need in the handle directory.

#![no_std]

use core::fmt::Write;

use oceans_rt::{Directory, Handle, Out, Start};

/// What a utility runs with.
pub struct Utility {
    pub out: Out,
    pub sysinfo: Handle,
}

/// Exit codes shared by the utilities.
pub const EXIT_NO_CONSOLE: i64 = 1;
pub const EXIT_NO_SYSINFO: i64 = 2;
pub const EXIT_FAILED: i64 = 3;
pub const EXIT_MISSING: i64 = 4;
pub const EXIT_USAGE: i64 = 5;

impl Utility {
    /// `Err(exit code)` (with a message, if possible) when a needed
    /// capability was not granted.
    pub fn start(name: &str, start: &Start) -> Result<Self, i64> {
        let directory = Directory::from_start(start).ok_or(EXIT_NO_CONSOLE)?;
        let mut out = Out::new(directory.find_kind("console").ok_or(EXIT_NO_CONSOLE)?);
        let Some(sysinfo) = directory.find_kind("sysinfo") else {
            let _ = writeln!(
                out,
                "{name}: needs the sysinfo capability (run {name} out sysinfo)"
            );
            return Err(EXIT_NO_SYSINFO);
        };
        Ok(Self { out, sysinfo })
    }
}

/// Console output and the handle directory, for utilities that need other
/// capabilities than `sysinfo`.
pub fn console(start: &Start) -> Result<(Out, Directory), i64> {
    let directory = Directory::from_start(start).ok_or(EXIT_NO_CONSOLE)?;
    let out = Out::new(directory.find_kind("console").ok_or(EXIT_NO_CONSOLE)?);
    Ok((out, directory))
}

/// The capability of `kind` called `name`, or a message saying how to
/// grant it and `Err(EXIT_MISSING)`.
pub fn require(
    out: &mut Out,
    directory: &Directory,
    program: &str,
    kind: &str,
    name: &str,
    grant: &str,
) -> Result<Handle, i64> {
    directory.find(kind, name).ok_or_else(|| {
        let _ = writeln!(
            out,
            "{program}: needs the {name} capability (run {program} out {grant} ...)"
        );
        EXIT_MISSING
    })
}

/// `bytes` as a short human-readable size.
pub struct Size(pub u64);

impl core::fmt::Display for Size {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        const KIB: u64 = 1024;
        const MIB: u64 = 1024 * KIB;
        let mut text = oceans_rt::Buffer::<24>::new();
        let _ = match self.0 {
            n if n >= 10 * MIB => write!(text, "{}M", n / MIB),
            n if n >= 10 * KIB => write!(text, "{}K", n / KIB),
            n => write!(text, "{n}B"),
        };
        // `pad` honours width and alignment (`{:>7}`).
        f.pad(text.as_str())
    }
}
