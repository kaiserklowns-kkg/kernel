//! Web bundles (ADR-0064): the files of a web app (a SvelteKit build) in
//! one file of its package, `web.bundle`. Package files have flat names of
//! at most 40 bytes; a web app's files live under paths.
//!
//! ```text
//! magic "OCEANSWB", version u32 = 1, count u32
//! count × { path_len u16, path, size u32, data }
//! ```
//!
//! Little-endian. Paths are relative (`index.html`, `_app/…/start.js`):
//! `/`-separated segments of ASCII letters, digits, `.`, `_`, `-`, `~`, `@`,
//! `+`, none empty, `.` or `..`, none starting with `.`. The whole package
//! is signed, so a bundle is only read once its signature verified; the
//! reader still checks everything (the bridge has its own, in Go).

pub const MAGIC: &[u8; 8] = b"OCEANSWB";
pub const VERSION: u32 = 1;
/// Most files in a bundle.
pub const MAX_FILES: usize = 2048;
/// Longest path.
pub const MAX_PATH: usize = 200;
/// Largest bundle: within a package's 32 MiB.
pub const MAX_BUNDLE: usize = 30 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BundleError {
    BadMagic,
    BadVersion,
    TooManyFiles,
    Truncated,
    BadPath,
    DuplicatePath,
    TooLarge,
    /// Bytes after the last file.
    Trailing,
}

/// Whether `path` may name a file in a bundle.
pub fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && !segment.starts_with('.')
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-~@+".contains(&b))
        })
}

/// Calls `each(path, data)` for every file, in order, after checking the
/// whole bundle.
pub fn read<'a>(
    bundle: &'a [u8],
    mut each: impl FnMut(&'a str, &'a [u8]),
) -> Result<(), BundleError> {
    if bundle.len() > MAX_BUNDLE {
        return Err(BundleError::TooLarge);
    }
    let rest = bundle.strip_prefix(MAGIC).ok_or(BundleError::BadMagic)?;
    let (version, rest) = take_u32(rest)?;
    if version != VERSION {
        return Err(BundleError::BadVersion);
    }
    let (count, mut rest) = take_u32(rest)?;
    if count as usize > MAX_FILES {
        return Err(BundleError::TooManyFiles);
    }
    // Checked whole first (pass 0), handed out after (pass 1): a bad
    // bundle yields nothing.
    let start = rest;
    for pass in 0..2 {
        rest = start;
        for index in 0..count {
            let (len, after) = take_u16(rest)?;
            let path = after.get(..len as usize).ok_or(BundleError::Truncated)?;
            let path = core::str::from_utf8(path).map_err(|_| BundleError::BadPath)?;
            if !valid_path(path) {
                return Err(BundleError::BadPath);
            }
            let (size, after) = take_u32(&after[len as usize..])?;
            let data = after.get(..size as usize).ok_or(BundleError::Truncated)?;
            rest = &after[size as usize..];
            if pass == 0 {
                // Duplicates: compare with every earlier path (bundles are
                // small; no allocation needed).
                if earlier_has(start, index, path)? {
                    return Err(BundleError::DuplicatePath);
                }
            } else {
                each(path, data);
            }
        }
        if pass == 0 && !rest.is_empty() {
            return Err(BundleError::Trailing);
        }
    }
    Ok(())
}

/// Whether one of the first `before` entries of the table at `table` is
/// `path`.
fn earlier_has(mut table: &[u8], before: u32, path: &str) -> Result<bool, BundleError> {
    for _ in 0..before {
        let (len, after) = take_u16(table)?;
        let other = after.get(..len as usize).ok_or(BundleError::Truncated)?;
        let (size, after) = take_u32(&after[len as usize..])?;
        if other == path.as_bytes() {
            return Ok(true);
        }
        table = after.get(size as usize..).ok_or(BundleError::Truncated)?;
    }
    Ok(false)
}

fn take_u16(bytes: &[u8]) -> Result<(u16, &[u8]), BundleError> {
    let head = bytes.get(..2).ok_or(BundleError::Truncated)?;
    Ok((u16::from_le_bytes([head[0], head[1]]), &bytes[2..]))
}

fn take_u32(bytes: &[u8]) -> Result<(u32, &[u8]), BundleError> {
    let head = bytes.get(..4).ok_or(BundleError::Truncated)?;
    Ok((
        u32::from_le_bytes([head[0], head[1], head[2], head[3]]),
        &bytes[4..],
    ))
}

/// A bundle of `files` (`(path, data)`), checked as [`read`] checks it.
#[cfg(feature = "build")]
pub fn write(files: &[(&str, &[u8])]) -> Result<alloc::vec::Vec<u8>, BundleError> {
    if files.len() > MAX_FILES {
        return Err(BundleError::TooManyFiles);
    }
    let mut out = alloc::vec::Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for (path, data) in files {
        if !valid_path(path) {
            return Err(BundleError::BadPath);
        }
        out.extend_from_slice(&(path.len() as u16).to_le_bytes());
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(
            &u32::try_from(data.len())
                .map_err(|_| BundleError::TooLarge)?
                .to_le_bytes(),
        );
        out.extend_from_slice(data);
    }
    // Duplicates and the size limit, as a reader sees them.
    read(&out, |_, _| {})?;
    Ok(out)
}
