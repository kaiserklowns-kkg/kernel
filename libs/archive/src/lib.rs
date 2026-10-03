//! The Oceans boot archive (ADR-0025): the system's programs and
//! configuration packed into one file, loaded by the boot loader as the
//! `initrd` module.
//!
//! ```text
//! header   magic "OCEANSAR", version u32 = 1, count u32, reserved [u8; 16]
//! table    count entries of 64 bytes:
//!          name [u8; 40] (UTF-8, zero-padded), offset u64, size u64,
//!          crc32c u32, reserved u32
//! data     each file at its offset (16-byte aligned), in table order
//! ```
//!
//! Little-endian throughout. The reader validates everything before handing
//! out a byte: the table is inside the archive, files are inside it and do
//! not overlap the table, names are valid and unique, and each file's
//! CRC-32C matches. No allocation, so the kernel and init can both read it.

#![no_std]

pub const MAGIC: &[u8; 8] = b"OCEANSAR";
pub const VERSION: u32 = 1;
const HEADER: usize = 32;
const ENTRY: usize = 64;
/// Longest file name.
pub const MAX_NAME: usize = 40;
/// Most files in one archive.
pub const MAX_FILES: usize = 256;
const ALIGN: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveError {
    BadMagic,
    BadVersion,
    TooManyFiles,
    Truncated,
    BadName,
    DuplicateName,
    /// A file lies outside the archive or over the table.
    BadExtent,
    /// A file's contents do not match its checksum.
    Corrupt,
    /// The output buffer has the wrong size (writing).
    WrongSize,
}

/// CRC-32C (Castagnoli).
pub fn crc32c(bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    (c >> 1) ^ 0x82f6_3b78
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };
    !bytes.iter().fold(!0u32, |c, &b| {
        TABLE[((c ^ u32::from(b)) & 0xff) as usize] ^ (c >> 8)
    })
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

fn valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && core::str::from_utf8(name).is_ok()
        && !name.contains(&b'/')
        && !name.contains(&0)
}

/// One file in the archive.
#[derive(Clone, Copy, Debug)]
pub struct File<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
}

/// A validated archive.
#[derive(Clone, Copy, Debug)]
pub struct Archive<'a> {
    bytes: &'a [u8],
    count: usize,
}

impl<'a> Archive<'a> {
    /// Validates the whole archive, including every file's checksum.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ArchiveError> {
        let header = bytes.get(..HEADER).ok_or(ArchiveError::Truncated)?;
        if &header[..8] != MAGIC {
            return Err(ArchiveError::BadMagic);
        }
        if u32_at(header, 8) != VERSION {
            return Err(ArchiveError::BadVersion);
        }
        let count = u32_at(header, 12) as usize;
        if count > MAX_FILES {
            return Err(ArchiveError::TooManyFiles);
        }
        let table_end = HEADER + count * ENTRY;
        if table_end > bytes.len() {
            return Err(ArchiveError::Truncated);
        }
        let archive = Self { bytes, count };
        for index in 0..count {
            let (name, offset, size, crc) = archive.entry(index);
            if !valid_name(name) {
                return Err(ArchiveError::BadName);
            }
            let end = offset.checked_add(size).ok_or(ArchiveError::BadExtent)?;
            if offset < table_end as u64 || end > bytes.len() as u64 {
                return Err(ArchiveError::BadExtent);
            }
            let data = &bytes[offset as usize..end as usize];
            if crc32c(data) != crc {
                return Err(ArchiveError::Corrupt);
            }
            if (0..index).any(|other| archive.entry(other).0 == name) {
                return Err(ArchiveError::DuplicateName);
            }
        }
        Ok(archive)
    }

    /// Raw table entry: (name bytes, offset, size, crc).
    fn entry(&self, index: usize) -> (&'a [u8], u64, u64, u32) {
        let at = HEADER + index * ENTRY;
        let entry = &self.bytes[at..at + ENTRY];
        let name = &entry[..MAX_NAME];
        let len = name.iter().position(|&b| b == 0).unwrap_or(MAX_NAME);
        (
            &name[..len],
            u64_at(entry, 40),
            u64_at(entry, 48),
            u32_at(entry, 56),
        )
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// File `index` (validated by `parse`).
    pub fn file(&self, index: usize) -> Option<File<'a>> {
        if index >= self.count {
            return None;
        }
        let (name, offset, size, _) = self.entry(index);
        Some(File {
            name: core::str::from_utf8(name).expect("validated"),
            data: &self.bytes[offset as usize..(offset + size) as usize],
        })
    }

    pub fn files(&self) -> impl Iterator<Item = File<'a>> + '_ {
        (0..self.count).filter_map(|i| self.file(i))
    }

    /// The contents of the file called `name`.
    pub fn find(&self, name: &str) -> Option<&'a [u8]> {
        self.files().find(|f| f.name == name).map(|f| f.data)
    }
}

/// Bytes an archive of `files` takes.
pub fn archive_len(files: &[(&str, &[u8])]) -> usize {
    let mut len = HEADER + files.len() * ENTRY;
    for (_, data) in files {
        len = len.next_multiple_of(ALIGN) + data.len();
    }
    len
}

/// Writes an archive of `files` into `out`, which must be exactly
/// [`archive_len`] bytes.
pub fn write(files: &[(&str, &[u8])], out: &mut [u8]) -> Result<(), ArchiveError> {
    if files.len() > MAX_FILES {
        return Err(ArchiveError::TooManyFiles);
    }
    if out.len() != archive_len(files) {
        return Err(ArchiveError::WrongSize);
    }
    out.fill(0);
    out[..8].copy_from_slice(MAGIC);
    out[8..12].copy_from_slice(&VERSION.to_le_bytes());
    out[12..16].copy_from_slice(&(files.len() as u32).to_le_bytes());
    let mut offset = HEADER + files.len() * ENTRY;
    for (index, (name, data)) in files.iter().enumerate() {
        if !valid_name(name.as_bytes()) {
            return Err(ArchiveError::BadName);
        }
        if files[..index].iter().any(|(other, _)| other == name) {
            return Err(ArchiveError::DuplicateName);
        }
        offset = offset.next_multiple_of(ALIGN);
        let at = HEADER + index * ENTRY;
        out[at..at + name.len()].copy_from_slice(name.as_bytes());
        out[at + 40..at + 48].copy_from_slice(&(offset as u64).to_le_bytes());
        out[at + 48..at + 56].copy_from_slice(&(data.len() as u64).to_le_bytes());
        out[at + 56..at + 60].copy_from_slice(&crc32c(data).to_le_bytes());
        out[offset..offset + data.len()].copy_from_slice(data);
        offset += data.len();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn build(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = vec![0u8; archive_len(files)];
        write(files, &mut out).unwrap();
        out
    }

    #[test]
    fn round_trips() {
        let files: [(&str, &[u8]); 3] = [
            ("init", b"\x7fELF init"),
            ("services.conf", b"service echo\n"),
            ("empty", b""),
        ];
        let bytes = build(&files);
        let archive = Archive::parse(&bytes).unwrap();
        assert_eq!(archive.len(), 3);
        for (name, data) in files {
            assert_eq!(archive.find(name), Some(data));
        }
        assert_eq!(archive.find("missing"), None);
        let names: Vec<&str> = archive.files().map(|f| f.name).collect();
        assert_eq!(names, ["init", "services.conf", "empty"]);
        assert!(Archive::parse(&build(&[])).unwrap().is_empty());
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn writer_refuses_bad_input() {
        let mut out = vec![0u8; archive_len(&[("a", b"1"), ("a", b"2")])];
        assert_eq!(
            write(&[("a", b"1"), ("a", b"2")], &mut out),
            Err(ArchiveError::DuplicateName)
        );
        for bad in [
            "",
            "a/b",
            "x\0y",
            "a-name-that-is-longer-than-forty-bytes-long",
        ] {
            let files: [(&str, &[u8]); 1] = [(bad, b"")];
            let mut out = vec![0u8; archive_len(&files)];
            assert_eq!(
                write(&files, &mut out),
                Err(ArchiveError::BadName),
                "{bad:?}"
            );
        }
        let mut short = [0u8; 10];
        assert_eq!(
            write(&[("a", b"1")], &mut short),
            Err(ArchiveError::WrongSize)
        );
    }

    #[test]
    fn reader_refuses_damage() {
        let good = build(&[("one", b"first file"), ("two", b"second")]);
        let mut magic = good.clone();
        magic[0] = b'X';
        assert_eq!(Archive::parse(&magic).err(), Some(ArchiveError::BadMagic));
        let mut version = good.clone();
        version[8] = 2;
        assert_eq!(
            Archive::parse(&version).err(),
            Some(ArchiveError::BadVersion)
        );
        let mut flipped = good.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert_eq!(Archive::parse(&flipped).err(), Some(ArchiveError::Corrupt));
        assert_eq!(
            Archive::parse(&good[..good.len() - 1]).err(),
            Some(ArchiveError::BadExtent)
        );
        assert_eq!(
            Archive::parse(&good[..40]).err(),
            Some(ArchiveError::Truncated)
        );
        let mut count = good.clone();
        count[12..16].copy_from_slice(&1000u32.to_le_bytes());
        assert_eq!(
            Archive::parse(&count).err(),
            Some(ArchiveError::TooManyFiles)
        );
        // A file pointing into the table.
        let mut overlap = good.clone();
        overlap[HEADER + 40..HEADER + 48].copy_from_slice(&0u64.to_le_bytes());
        assert_eq!(
            Archive::parse(&overlap).err(),
            Some(ArchiveError::BadExtent)
        );
        // An offset that overflows.
        let mut overflow = good.clone();
        overflow[HEADER + 40..HEADER + 48].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            Archive::parse(&overflow).err(),
            Some(ArchiveError::BadExtent)
        );
        // The same name twice.
        let mut duplicate = good.clone();
        duplicate[HEADER + ENTRY..HEADER + ENTRY + 3].copy_from_slice(b"one");
        assert_eq!(
            Archive::parse(&duplicate).err(),
            Some(ArchiveError::DuplicateName)
        );
        // Every single-byte mutation: an error or a valid archive, never a
        // panic.
        for at in 0..good.len() {
            for value in [0u8, 0xff, 0x41] {
                let mut mutated = good.clone();
                mutated[at] = value;
                if let Ok(archive) = Archive::parse(&mutated) {
                    let _ = archive.files().count();
                }
            }
        }
    }
}
