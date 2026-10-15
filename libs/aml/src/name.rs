//! Names in the ACPI namespace (ACPI 6.5 §5.3, §20.2.2): four-character
//! segments, paths from the root, and names as AML writes them (from the
//! root, up some levels with `^`, or relative to the scope).

use alloc::vec::Vec;
use core::fmt;

/// One name segment: four characters, `_` padded.
pub type Seg = [u8; 4];

/// A path from the root (`\`), segment by segment. The root is empty.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Path(pub Vec<Seg>);

impl Path {
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// `\A.B.C` style text: segments of up to four characters, `_` padded.
    /// `None` if it is not such a path.
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix('\\')?;
        if rest.is_empty() {
            return Some(Self::root());
        }
        let mut segs = Vec::new();
        for part in rest.split('.') {
            let bytes = part.as_bytes();
            if bytes.is_empty() || bytes.len() > 4 {
                return None;
            }
            let mut seg = *b"____";
            seg[..bytes.len()].copy_from_slice(bytes);
            if !valid_seg(&seg) {
                return None;
            }
            segs.push(seg);
        }
        Some(Self(segs))
    }

    /// The path one level down.
    pub fn child(&self, seg: Seg) -> Self {
        let mut segs = self.0.clone();
        segs.push(seg);
        Self(segs)
    }

    /// The path one level up (the root stays the root).
    pub fn parent(&self) -> Self {
        let mut segs = self.0.clone();
        segs.pop();
        Self(segs)
    }

    /// The last segment (`None` for the root).
    pub fn last(&self) -> Option<Seg> {
        self.0.last().copied()
    }

    pub fn depth(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("\\")?;
        for (i, seg) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            for &b in seg {
                write!(f, "{}", char::from(b))?;
            }
        }
        Ok(())
    }
}

/// Whether `seg` is a valid name segment: a lead character `A`-`Z` or
/// `_`, then `A`-`Z`, `0`-`9` or `_`.
pub fn valid_seg(seg: &Seg) -> bool {
    let lead = |b: u8| b.is_ascii_uppercase() || b == b'_';
    lead(seg[0]) && seg[1..].iter().all(|&b| lead(b) || b.is_ascii_digit())
}

/// A name as AML writes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameString {
    /// From the root (`\`).
    pub root: bool,
    /// Levels up (`^` each), when not from the root.
    pub up: u8,
    pub segs: Vec<Seg>,
}

impl NameString {
    /// The null name (`NullName`): no segments.
    pub fn is_null(&self) -> bool {
        !self.root && self.up == 0 && self.segs.is_empty()
    }

    /// Whether the namespace's search rules apply: a single segment with
    /// no prefix is looked for in the scope, then in each one above it
    /// (ACPI 6.5 §5.3).
    pub fn searches_up(&self) -> bool {
        !self.root && self.up == 0 && self.segs.len() == 1
    }

    /// The path it names in `scope`, as written (no search): `None` if it
    /// goes above the root.
    pub fn resolve(&self, scope: &Path) -> Option<Path> {
        let mut segs = if self.root {
            Vec::new()
        } else {
            let keep = scope.0.len().checked_sub(usize::from(self.up))?;
            scope.0[..keep].to_vec()
        };
        segs.extend_from_slice(&self.segs);
        Some(Path(segs))
    }
}

impl fmt::Display for NameString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.root {
            f.write_str("\\")?;
        }
        for _ in 0..self.up {
            f.write_str("^")?;
        }
        for (i, seg) in self.segs.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            for &b in seg {
                write!(f, "{}", char::from(b))?;
            }
        }
        Ok(())
    }
}

/// A compressed EISA ID (`EisaId ("PNP0C0A")`, ACPI 6.5 §19.6.47) as
/// text: three letters and four hex digits.
pub fn eisa_id(value: u32) -> [u8; 7] {
    let v = value.swap_bytes();
    let letter = |shift: u32| b'@' + ((v >> shift) & 0x1f) as u8;
    let hex = |shift: u32| b"0123456789ABCDEF"[((v >> shift) & 0xf) as usize];
    [
        letter(26),
        letter(21),
        letter(16),
        hex(12),
        hex(8),
        hex(4),
        hex(0),
    ]
}
