//! Strict parser for static ELF64 x86_64 executables (ADR-0014).
//!
//! Untrusted input: every offset and size is bounds- and overflow-checked
//! before use, and the policy checks of the Oceans loader are applied here,
//! so a successfully parsed [`Executable`] is safe to load:
//!
//! - static `ET_EXEC` only (no interpreter, no dynamic section);
//! - every `PT_LOAD` segment inside the allowed address range, non-empty,
//!   with file data within the image and not larger than its memory size;
//! - no segment both writable and executable (W^X for userspace);
//! - segments do not overlap (at page granularity);
//! - the entry point lies in an executable segment;
//! - total memory bounded.
//!
//! No allocation; the parsed view borrows the image.

#![no_std]

pub const PAGE_SIZE: u64 = 4096;
const MAX_PROGRAM_HEADERS: usize = 64;

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const CLASS_64: u8 = 2;
const DATA_LITTLE_ENDIAN: u8 = 1;
const VERSION_CURRENT: u8 = 1;
const TYPE_EXEC: u16 = 2;
const MACHINE_X86_64: u16 = 0x3e;
const HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;

const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElfError {
    /// Shorter than a header, or headers point outside the image.
    Truncated,
    /// Not an ELF file, or not ELF64 little-endian version 1.
    NotElf,
    /// Not a static x86_64 executable.
    Unsupported,
    /// Program headers malformed (size, count).
    BadProgramHeaders,
    /// A segment's file data lies outside the image, or exceeds its size.
    BadSegment { index: usize },
    /// A segment lies outside the allowed range or overflows.
    SegmentOutOfRange { index: usize },
    /// A segment is both writable and executable.
    WritableAndExecutable { index: usize },
    /// Two segments share a page.
    OverlappingSegments { first: usize, second: usize },
    /// No loadable segment.
    NoSegments,
    /// The entry point is not in an executable segment.
    BadEntryPoint,
    /// Segments together exceed the memory limit.
    TooLarge,
}

/// Where and how much a program may occupy.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Lowest allowed address (keeps page 0 unmapped).
    pub lowest: u64,
    /// Exclusive upper bound.
    pub highest: u64,
    /// Maximum sum of segment memory sizes.
    pub max_total: u64,
}

/// A loadable segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment<'a> {
    pub vaddr: u64,
    pub mem_size: u64,
    /// Bytes to copy to `vaddr`; the rest up to `mem_size` is zero.
    pub data: &'a [u8],
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
}

impl Segment<'_> {
    /// Page-aligned start of the pages the segment occupies.
    pub fn page_start(&self) -> u64 {
        self.vaddr - self.vaddr % PAGE_SIZE
    }

    /// Page-aligned end (exclusive). Cannot overflow: checked by `parse`.
    pub fn page_end(&self) -> u64 {
        (self.vaddr + self.mem_size).next_multiple_of(PAGE_SIZE)
    }
}

/// A validated executable image.
#[derive(Clone, Copy, Debug)]
pub struct Executable<'a> {
    image: &'a [u8],
    entry: u64,
    program_headers: usize,
    program_header_count: usize,
}

impl<'a> Executable<'a> {
    pub fn parse(image: &'a [u8], limits: Limits) -> Result<Self, ElfError> {
        let header = image.get(..HEADER_SIZE).ok_or(ElfError::Truncated)?;
        if header[..4] != ELF_MAGIC
            || header[4] != CLASS_64
            || header[5] != DATA_LITTLE_ENDIAN
            || header[6] != VERSION_CURRENT
        {
            return Err(ElfError::NotElf);
        }
        if u16_at(header, 16) != TYPE_EXEC
            || u16_at(header, 18) != MACHINE_X86_64
            || u32_at(header, 20) != 1
        {
            return Err(ElfError::Unsupported);
        }
        let entry = u64_at(header, 24);
        let program_headers =
            usize::try_from(u64_at(header, 32)).map_err(|_| ElfError::Truncated)?;
        let entry_size = usize::from(u16_at(header, 54));
        let count = usize::from(u16_at(header, 56));
        if entry_size != PROGRAM_HEADER_SIZE || count > MAX_PROGRAM_HEADERS {
            return Err(ElfError::BadProgramHeaders);
        }
        let table_end = count
            .checked_mul(PROGRAM_HEADER_SIZE)
            .and_then(|size| program_headers.checked_add(size))
            .ok_or(ElfError::Truncated)?;
        if table_end > image.len() {
            return Err(ElfError::Truncated);
        }

        let executable = Self {
            image,
            entry,
            program_headers,
            program_header_count: count,
        };
        executable.validate(limits)?;
        Ok(executable)
    }

    pub fn entry(&self) -> u64 {
        self.entry
    }

    /// The `PT_LOAD` segments, in file order.
    pub fn segments(&self) -> impl Iterator<Item = Segment<'a>> + '_ {
        (0..self.program_header_count).filter_map(move |i| match self.raw_header(i) {
            RawHeader::Load(segment) => Some(segment),
            _ => None,
        })
    }

    fn validate(&self, limits: Limits) -> Result<(), ElfError> {
        let mut total: u64 = 0;
        let mut loads = 0;
        for index in 0..self.program_header_count {
            let segment = match self.checked_header(index, limits)? {
                Some(segment) => segment,
                None => continue,
            };
            loads += 1;
            total = total
                .checked_add(segment.mem_size)
                .ok_or(ElfError::TooLarge)?;
            for earlier in 0..index {
                if let Some(other) = self.checked_header(earlier, limits)?
                    && segment.page_start() < other.page_end()
                    && other.page_start() < segment.page_end()
                {
                    return Err(ElfError::OverlappingSegments {
                        first: earlier,
                        second: index,
                    });
                }
            }
        }
        if loads == 0 {
            return Err(ElfError::NoSegments);
        }
        if total > limits.max_total {
            return Err(ElfError::TooLarge);
        }
        let entry_ok = self
            .segments()
            .any(|s| s.executable && (s.vaddr..s.vaddr + s.mem_size).contains(&self.entry));
        if !entry_ok {
            return Err(ElfError::BadEntryPoint);
        }
        Ok(())
    }

    /// Validates header `index`; `None` for headers that are ignored.
    fn checked_header(
        &self,
        index: usize,
        limits: Limits,
    ) -> Result<Option<Segment<'a>>, ElfError> {
        let raw = self.header_bytes(index);
        let kind = u32_at(raw, 0);
        match kind {
            PT_INTERP | PT_DYNAMIC => return Err(ElfError::Unsupported),
            PT_LOAD => {}
            _ => return Ok(None),
        }
        let flags = u32_at(raw, 4);
        let offset = u64_at(raw, 8);
        let vaddr = u64_at(raw, 16);
        let file_size = u64_at(raw, 32);
        let mem_size = u64_at(raw, 40);

        if file_size > mem_size {
            return Err(ElfError::BadSegment { index });
        }
        if mem_size == 0 {
            // Linkers may emit empty segments; they load nothing.
            return Ok(None);
        }
        let file_end = offset
            .checked_add(file_size)
            .ok_or(ElfError::BadSegment { index })?;
        if file_end > self.image.len() as u64 {
            return Err(ElfError::BadSegment { index });
        }
        let end = vaddr.checked_add(mem_size);
        let in_range = vaddr >= limits.lowest
            && end.is_some_and(|end| end.next_multiple_of(PAGE_SIZE) <= limits.highest);
        if !in_range {
            return Err(ElfError::SegmentOutOfRange { index });
        }
        if flags & PF_W != 0 && flags & PF_X != 0 {
            return Err(ElfError::WritableAndExecutable { index });
        }
        Ok(Some(Segment {
            vaddr,
            mem_size,
            data: &self.image[offset as usize..file_end as usize],
            readable: flags & PF_R != 0,
            writable: flags & PF_W != 0,
            executable: flags & PF_X != 0,
        }))
    }

    fn header_bytes(&self, index: usize) -> &'a [u8] {
        let start = self.program_headers + index * PROGRAM_HEADER_SIZE;
        // In bounds: checked against `table_end` in `parse`.
        &self.image[start..start + PROGRAM_HEADER_SIZE]
    }

    fn raw_header(&self, index: usize) -> RawHeader<'a> {
        let raw = self.header_bytes(index);
        if u32_at(raw, 0) != PT_LOAD || u64_at(raw, 40) == 0 {
            return RawHeader::Other;
        }
        let flags = u32_at(raw, 4);
        let offset = u64_at(raw, 8) as usize;
        let file_size = u64_at(raw, 32) as usize;
        // Validated by `checked_header` during `parse`.
        RawHeader::Load(Segment {
            vaddr: u64_at(raw, 16),
            mem_size: u64_at(raw, 40),
            data: &self.image[offset..offset + file_size],
            readable: flags & PF_R != 0,
            writable: flags & PF_W != 0,
            executable: flags & PF_X != 0,
        })
    }
}

enum RawHeader<'a> {
    Load(Segment<'a>),
    Other,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec;
    use std::vec::Vec;

    const LIMITS: Limits = Limits {
        lowest: 0x1000,
        highest: 0x0000_8000_0000_0000,
        max_total: 1 << 30,
    };

    struct Ph {
        kind: u32,
        flags: u32,
        offset: u64,
        vaddr: u64,
        file_size: u64,
        mem_size: u64,
    }

    fn load(flags: u32, offset: u64, vaddr: u64, file_size: u64, mem_size: u64) -> Ph {
        Ph {
            kind: PT_LOAD,
            flags,
            offset,
            vaddr,
            file_size,
            mem_size,
        }
    }

    fn build(entry: u64, headers: &[Ph], payload: usize) -> Vec<u8> {
        let phoff = HEADER_SIZE;
        let mut image = vec![0u8; phoff + headers.len() * PROGRAM_HEADER_SIZE + payload];
        image[..4].copy_from_slice(&ELF_MAGIC);
        image[4] = CLASS_64;
        image[5] = DATA_LITTLE_ENDIAN;
        image[6] = VERSION_CURRENT;
        image[16..18].copy_from_slice(&TYPE_EXEC.to_le_bytes());
        image[18..20].copy_from_slice(&MACHINE_X86_64.to_le_bytes());
        image[20..24].copy_from_slice(&1u32.to_le_bytes());
        image[24..32].copy_from_slice(&entry.to_le_bytes());
        image[32..40].copy_from_slice(&(phoff as u64).to_le_bytes());
        image[54..56].copy_from_slice(&(PROGRAM_HEADER_SIZE as u16).to_le_bytes());
        image[56..58].copy_from_slice(&(headers.len() as u16).to_le_bytes());
        for (i, ph) in headers.iter().enumerate() {
            let at = phoff + i * PROGRAM_HEADER_SIZE;
            image[at..at + 4].copy_from_slice(&ph.kind.to_le_bytes());
            image[at + 4..at + 8].copy_from_slice(&ph.flags.to_le_bytes());
            image[at + 8..at + 16].copy_from_slice(&ph.offset.to_le_bytes());
            image[at + 16..at + 24].copy_from_slice(&ph.vaddr.to_le_bytes());
            image[at + 32..at + 40].copy_from_slice(&ph.file_size.to_le_bytes());
            image[at + 40..at + 48].copy_from_slice(&ph.mem_size.to_le_bytes());
        }
        image
    }

    fn typical() -> Vec<u8> {
        build(
            0x40_1000,
            &[
                load(PF_R | PF_X, 0, 0x40_1000, 0x100, 0x100),
                load(PF_R, 0, 0x40_2000, 0x10, 0x10),
                load(PF_R | PF_W, 0, 0x40_3000, 0x20, 0x2000),
            ],
            0x200,
        )
    }

    #[test]
    fn parses_typical_executable() {
        let image = typical();
        let exe = Executable::parse(&image, LIMITS).unwrap();
        assert_eq!(exe.entry(), 0x40_1000);
        let segments: Vec<_> = exe.segments().collect();
        assert_eq!(segments.len(), 3);
        assert!(segments[0].executable && !segments[0].writable);
        assert_eq!(segments[2].data.len(), 0x20);
        assert_eq!(segments[2].page_end(), 0x40_5000, "bss rounded to pages");
    }

    #[test]
    fn rejects_non_elf_and_unsupported() {
        assert_eq!(
            Executable::parse(&[0; 10], LIMITS).err(),
            Some(ElfError::Truncated)
        );
        let mut image = typical();
        image[0] = 0;
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::NotElf)
        );
        let mut image = typical();
        image[4] = 1; // ELF32
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::NotElf)
        );
        let mut image = typical();
        image[16] = 3; // ET_DYN
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::Unsupported)
        );
        let image = build(
            0x40_1000,
            &[
                load(PF_R | PF_X, 0, 0x40_1000, 1, 1),
                Ph {
                    kind: PT_INTERP,
                    flags: 0,
                    offset: 0,
                    vaddr: 0,
                    file_size: 0,
                    mem_size: 0,
                },
            ],
            16,
        );
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::Unsupported)
        );
    }

    #[test]
    fn rejects_out_of_bounds_file_data() {
        let image = build(
            0x40_1000,
            &[load(PF_R | PF_X, 0x100, 0x40_1000, 0x1000, 0x1000)],
            0x10,
        );
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::BadSegment { index: 0 })
        );
        let image = build(
            0x40_1000,
            &[load(PF_R | PF_X, u64::MAX - 1, 0x40_1000, 4, 4)],
            0x10,
        );
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::BadSegment { index: 0 })
        );
        let image = build(0x40_1000, &[load(PF_R | PF_X, 0, 0x40_1000, 8, 4)], 0x10);
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::BadSegment { index: 0 })
        );
    }

    #[test]
    fn rejects_segments_outside_user_space() {
        for vaddr in [0, 0xfff, 0xffff_8000_0000_0000, 0x0000_7fff_ffff_f000] {
            let image = build(vaddr, &[load(PF_R | PF_X, 0, vaddr, 0, 0x2000)], 0);
            assert_eq!(
                Executable::parse(&image, LIMITS).err(),
                Some(ElfError::SegmentOutOfRange { index: 0 }),
                "vaddr {vaddr:#x}"
            );
        }
        let image = build(
            0x40_1000,
            &[load(PF_R | PF_X, 0, u64::MAX - 0x10, 0, 0x100)],
            0,
        );
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::SegmentOutOfRange { index: 0 })
        );
    }

    #[test]
    fn enforces_w_xor_x_overlap_and_entry() {
        let image = build(
            0x40_1000,
            &[load(PF_R | PF_W | PF_X, 0, 0x40_1000, 0, 0x10)],
            0,
        );
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::WritableAndExecutable { index: 0 })
        );
        let image = build(
            0x40_1000,
            &[
                load(PF_R | PF_X, 0, 0x40_1000, 0, 0x10),
                load(PF_R | PF_W, 0, 0x40_1800, 0, 0x10),
            ],
            0,
        );
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::OverlappingSegments {
                first: 0,
                second: 1
            })
        );
        let image = build(
            0x40_2000,
            &[
                load(PF_R | PF_X, 0, 0x40_1000, 0, 0x10),
                load(PF_R | PF_W, 0, 0x40_2000, 0, 0x10),
            ],
            0,
        );
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::BadEntryPoint),
            "entry in data"
        );
        let image = build(0x40_1000, &[], 0);
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::NoSegments)
        );
    }

    #[test]
    fn ignores_empty_segments() {
        let image = build(
            0x40_1000,
            &[
                load(PF_R, 0, 0x40_0000, 0, 0),
                load(PF_R | PF_X, 0, 0x40_1000, 4, 4),
            ],
            8,
        );
        let exe = Executable::parse(&image, LIMITS).unwrap();
        assert_eq!(exe.segments().count(), 1);
    }

    #[test]
    fn enforces_total_size_and_header_sanity() {
        let limits = Limits {
            max_total: 0x1000,
            ..LIMITS
        };
        let image = build(0x40_1000, &[load(PF_R | PF_X, 0, 0x40_1000, 0, 0x2000)], 0);
        assert_eq!(
            Executable::parse(&image, limits).err(),
            Some(ElfError::TooLarge)
        );
        let mut image = typical();
        image[54] = 32; // wrong program header size
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::BadProgramHeaders)
        );
        let mut image = typical();
        image[32..40].copy_from_slice(&(u64::MAX - 8).to_le_bytes());
        assert_eq!(
            Executable::parse(&image, LIMITS).err(),
            Some(ElfError::Truncated)
        );
    }
}
