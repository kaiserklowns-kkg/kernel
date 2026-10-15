//! Reading AML (ACPI 6.5 §20): package lengths, names, data, and every
//! opcode's operands, into expressions.
//!
//! AML is untrusted (firmware writes it): every read is bounds-checked,
//! every length must fit what holds it, and nesting is limited, so a
//! damaged table is an error, never a panic or a loop.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::name::{NameString, Seg, valid_seg};

/// Why AML could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// It ends in the middle of something, at this offset.
    Truncated(usize),
    /// An opcode this reader does not know, at this offset.
    UnknownOpcode(usize, u16),
    /// Something malformed (a bad name, a length past its container).
    Malformed(usize),
    /// Nested deeper than `MAX_DEPTH`.
    TooDeep(usize),
}

impl Error {
    /// Where in the AML it happened.
    pub fn offset(&self) -> usize {
        match *self {
            Self::Truncated(at)
            | Self::UnknownOpcode(at, _)
            | Self::Malformed(at)
            | Self::TooDeep(at) => at,
        }
    }
}

/// How deep expressions and blocks may nest.
pub const MAX_DEPTH: usize = 64;

/// Extended opcodes: `0x5B`, then a byte; written here as `0x5Bxx`.
pub const EXT: u16 = 0x5b00;

pub mod op {
    pub const ZERO: u16 = 0x00;
    pub const ONE: u16 = 0x01;
    pub const ALIAS: u16 = 0x06;
    pub const NAME: u16 = 0x08;
    pub const BYTE: u16 = 0x0a;
    pub const WORD: u16 = 0x0b;
    pub const DWORD: u16 = 0x0c;
    pub const STRING: u16 = 0x0d;
    pub const QWORD: u16 = 0x0e;
    pub const SCOPE: u16 = 0x10;
    pub const BUFFER: u16 = 0x11;
    pub const PACKAGE: u16 = 0x12;
    pub const VAR_PACKAGE: u16 = 0x13;
    pub const METHOD: u16 = 0x14;
    pub const EXTERNAL: u16 = 0x15;
    pub const DUAL_NAME: u8 = 0x2e;
    pub const MULTI_NAME: u8 = 0x2f;
    pub const ROOT: u8 = b'\\';
    pub const PARENT: u8 = b'^';
    pub const LOCAL0: u16 = 0x60;
    pub const LOCAL7: u16 = 0x67;
    pub const ARG0: u16 = 0x68;
    pub const ARG6: u16 = 0x6e;
    pub const IF: u16 = 0xa0;
    pub const ELSE: u16 = 0xa1;
    pub const WHILE: u16 = 0xa2;
    pub const ONES: u16 = 0xff;
    pub const MUTEX: u16 = 0x5b01;
    pub const EVENT: u16 = 0x5b02;
    pub const REVISION: u16 = 0x5b30;
    pub const DEBUG: u16 = 0x5b31;
    pub const REGION: u16 = 0x5b80;
    pub const FIELD: u16 = 0x5b81;
    pub const DEVICE: u16 = 0x5b82;
    pub const PROCESSOR: u16 = 0x5b83;
    pub const POWER_RES: u16 = 0x5b84;
    pub const THERMAL_ZONE: u16 = 0x5b85;
    pub const INDEX_FIELD: u16 = 0x5b86;
    pub const BANK_FIELD: u16 = 0x5b87;
    pub const DATA_REGION: u16 = 0x5b88;
}

/// How an opcode's operand is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    /// A `TermArg`: an expression.
    Term,
    /// A `SuperName`: where a value goes, or what is referred to.
    Super,
    /// A `Target`: a `SuperName`, or the null name (nowhere).
    Target,
    /// A `NameString` being defined.
    Name,
    Byte,
    Word,
    DWord,
}

use Operand::{Byte, DWord, Name, Super, Target, Term, Word};

/// The operands of the opcodes that are plain sequences of them (not
/// data, names, locals or arguments, nor anything with a package length).
pub fn operands(opcode: u16) -> Option<&'static [Operand]> {
    Some(match opcode {
        0x70 => &[Term, Super],                          // Store
        0x71 => &[Super],                                // RefOf
        0x72 => &[Term, Term, Target],                   // Add
        0x73 => &[Term, Term, Target],                   // Concatenate
        0x74 => &[Term, Term, Target],                   // Subtract
        0x75 | 0x76 => &[Super],                         // Increment, Decrement
        0x77 => &[Term, Term, Target],                   // Multiply
        0x78 => &[Term, Term, Target, Target],           // Divide
        0x79..=0x7f => &[Term, Term, Target],            // shifts, And, Nand, Or, Nor, Xor
        0x80..=0x82 => &[Term, Target],                  // Not, FindSetLeft/RightBit
        0x83 => &[Term],                                 // DerefOf
        0x84 | 0x85 => &[Term, Term, Target],            // ConcatenateResTemplate, Mod
        0x86 => &[Super, Term],                          // Notify
        0x87 | 0x8e => &[Super],                         // SizeOf, ObjectType
        0x88 => &[Term, Term, Target],                   // Index
        0x89 => &[Term, Byte, Term, Byte, Term, Term],   // Match
        0x8a..=0x8d | 0x8f => &[Term, Term, Name],       // CreateDWord/Word/Byte/Bit/QWordField
        0x90 | 0x91 => &[Term, Term],                    // LAnd, LOr
        0x92 => &[Term],                                 // LNot
        0x93..=0x95 => &[Term, Term],                    // LEqual, LGreater, LLess
        0x96..=0x99 => &[Term, Target],                  // ToBuffer, ToDecimal/HexString, ToInteger
        0x9c => &[Term, Term, Target],                   // ToString
        0x9d => &[Term, Super],                          // CopyObject
        0x9e => &[Term, Term, Term, Target],             // Mid
        0x9f | 0xa3 | 0xa5 | 0xcc => &[],                // Continue, Noop, Break, BreakPoint
        0xa4 => &[Term],                                 // Return
        0x5b12 => &[Super, Target],                      // CondRefOf
        0x5b13 => &[Term, Term, Term, Name],             // CreateField
        0x5b1f => &[Term, Term, Term, Term, Term, Term], // LoadTable
        0x5b20 => &[Name, Target],                       // Load
        0x5b21 | 0x5b22 => &[Term],                      // Stall, Sleep
        0x5b23 => &[Super, Word],                        // Acquire
        0x5b24 | 0x5b26 | 0x5b27 | 0x5b2a => &[Super],   // Signal, Reset, Release, Unload
        0x5b25 => &[Super, Term],                        // Wait
        0x5b28 | 0x5b29 => &[Term, Target],              // FromBCD, ToBCD
        0x5b32 => &[Byte, DWord, Term],                  // Fatal
        0x5b33 => &[],                                   // Timer
        _ => return None,
    })
}

/// An expression (a `TermArg`, a `SuperName` or a data object).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Int(u64),
    /// The bytes of a string, without its terminating zero.
    Str(Vec<u8>),
    /// A buffer: its size, and the bytes it starts with.
    Buffer(Box<Expr>, Vec<u8>),
    /// A package: its element count, and the elements given.
    Package(Box<Expr>, Vec<Expr>),
    /// A name, as a reference.
    Name(NameString),
    /// A method called with these arguments.
    Call(NameString, Vec<Expr>),
    Local(u8),
    Arg(u8),
    Debug,
    /// The null target: the result goes nowhere.
    Nothing,
    /// Any other opcode and its operands (bytes, words and dwords as
    /// `Int`, names defined as `Name`).
    Op(u16, Vec<Expr>),
}

/// The number of arguments the method at a name takes, if it names one:
/// what a call needs to be read (AML does not mark calls).
pub trait Arity {
    fn arity(&self, name: &NameString) -> Option<u8>;
}

/// No methods known: every name is a reference.
pub struct NoMethods;

impl Arity for NoMethods {
    fn arity(&self, _: &NameString) -> Option<u8> {
        None
    }
}

/// A cursor over AML.
pub struct Reader<'a> {
    pub bytes: &'a [u8],
    pub at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub fn done(&self) -> bool {
        self.at >= self.bytes.len()
    }

    pub fn peek(&self) -> Result<u8, Error> {
        self.bytes
            .get(self.at)
            .copied()
            .ok_or(Error::Truncated(self.at))
    }

    pub fn byte(&mut self) -> Result<u8, Error> {
        let b = self.peek()?;
        self.at += 1;
        Ok(b)
    }

    pub fn take(&mut self, len: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(len).ok_or(Error::Truncated(self.at))?;
        let bytes = self
            .bytes
            .get(self.at..end)
            .ok_or(Error::Truncated(self.at))?;
        self.at = end;
        Ok(bytes)
    }

    fn le(&mut self, len: usize) -> Result<u64, Error> {
        Ok(self
            .take(len)?
            .iter()
            .rev()
            .fold(0u64, |v, &b| (v << 8) | u64::from(b)))
    }

    /// The opcode at the cursor (one byte, or `0x5B` and one).
    pub fn opcode(&mut self) -> Result<u16, Error> {
        let first = self.byte()?;
        if first == 0x5b {
            Ok(EXT | u16::from(self.byte()?))
        } else {
            Ok(u16::from(first))
        }
    }

    /// A `PkgLength` (§20.2.4): returns where the package ends. The length
    /// counts its own bytes, and must stay within `limit`.
    pub fn pkg_length(&mut self, limit: usize) -> Result<usize, Error> {
        let start = self.at;
        let lead = self.byte()?;
        let follow = usize::from(lead >> 6);
        let length = if follow == 0 {
            usize::from(lead & 0x3f)
        } else {
            let mut length = usize::from(lead & 0x0f);
            for i in 0..follow {
                length |= usize::from(self.byte()?) << (4 + 8 * i);
            }
            length
        };
        let end = start.checked_add(length).ok_or(Error::Malformed(start))?;
        if end < self.at || end > limit {
            return Err(Error::Malformed(start));
        }
        Ok(end)
    }

    fn seg(&mut self) -> Result<Seg, Error> {
        let at = self.at;
        let seg: Seg = self.take(4)?.try_into().expect("four bytes");
        if valid_seg(&seg) {
            Ok(seg)
        } else {
            Err(Error::Malformed(at))
        }
    }

    /// Whether a name starts at the cursor.
    pub fn at_name(&self) -> bool {
        matches!(
            self.peek(),
            Ok(b'A'..=b'Z' | b'_' | op::ROOT | op::PARENT | op::DUAL_NAME | op::MULTI_NAME)
        )
    }

    /// A `NameString` (§20.2.2).
    pub fn name(&mut self) -> Result<NameString, Error> {
        let mut name = NameString::default();
        if self.peek()? == op::ROOT {
            self.at += 1;
            name.root = true;
        } else {
            while self.peek()? == op::PARENT {
                self.at += 1;
                name.up = name.up.checked_add(1).ok_or(Error::Malformed(self.at))?;
            }
        }
        match self.peek()? {
            0x00 => self.at += 1,
            op::DUAL_NAME => {
                self.at += 1;
                name.segs.push(self.seg()?);
                name.segs.push(self.seg()?);
            }
            op::MULTI_NAME => {
                self.at += 1;
                let count = self.byte()?;
                for _ in 0..count {
                    name.segs.push(self.seg()?);
                }
            }
            _ => name.segs.push(self.seg()?),
        }
        Ok(name)
    }

    /// A string (`StringPrefix`, ASCII to a zero).
    fn string(&mut self) -> Result<Vec<u8>, Error> {
        let start = self.at;
        let rest = self.bytes.get(start..).unwrap_or(&[]);
        let len = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(Error::Truncated(start))?;
        let text = rest[..len].to_vec();
        self.at = start + len + 1;
        Ok(text)
    }

    /// A `TermArg`. Names are calls if `methods` knows them as methods.
    pub fn term(&mut self, methods: &dyn Arity, depth: usize) -> Result<Expr, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::TooDeep(self.at));
        }
        if self.at_name() {
            let name = self.name()?;
            return Ok(match methods.arity(&name) {
                Some(count) => {
                    let mut args = Vec::new();
                    for _ in 0..count {
                        args.push(self.term(methods, depth + 1)?);
                    }
                    Expr::Call(name, args)
                }
                None => Expr::Name(name),
            });
        }
        let at = self.at;
        let opcode = self.opcode()?;
        Ok(match opcode {
            op::ZERO => Expr::Int(0),
            op::ONE => Expr::Int(1),
            op::ONES => Expr::Int(u64::MAX),
            op::BYTE => Expr::Int(self.le(1)?),
            op::WORD => Expr::Int(self.le(2)?),
            op::DWORD => Expr::Int(self.le(4)?),
            op::QWORD => Expr::Int(self.le(8)?),
            op::STRING => Expr::Str(self.string()?),
            op::REVISION => Expr::Op(op::REVISION, Vec::new()),
            op::DEBUG => Expr::Debug,
            op::LOCAL0..=op::LOCAL7 => Expr::Local((opcode - op::LOCAL0) as u8),
            op::ARG0..=op::ARG6 => Expr::Arg((opcode - op::ARG0) as u8),
            op::BUFFER => {
                let end = self.pkg_length(self.bytes.len())?;
                let size = self.term(methods, depth + 1)?;
                if self.at > end {
                    return Err(Error::Malformed(at));
                }
                let init = self.bytes[self.at..end].to_vec();
                self.at = end;
                Expr::Buffer(Box::new(size), init)
            }
            op::PACKAGE | op::VAR_PACKAGE => {
                let end = self.pkg_length(self.bytes.len())?;
                let count = if opcode == op::PACKAGE {
                    Expr::Int(u64::from(self.byte()?))
                } else {
                    self.term(methods, depth + 1)?
                };
                let mut elements = Vec::new();
                while self.at < end {
                    // A name in a package is a reference, never a call.
                    let element = if self.at_name() {
                        Expr::Name(self.name()?)
                    } else {
                        self.term(methods, depth + 1)?
                    };
                    elements.push(element);
                }
                if self.at != end {
                    return Err(Error::Malformed(at));
                }
                Expr::Package(Box::new(count), elements)
            }
            other => {
                let shape = operands(other).ok_or(Error::UnknownOpcode(at, other))?;
                let mut args = Vec::with_capacity(shape.len());
                for operand in shape {
                    args.push(self.operand(*operand, methods, depth + 1)?);
                }
                Expr::Op(other, args)
            }
        })
    }

    /// One operand of the given encoding.
    pub fn operand(
        &mut self,
        operand: Operand,
        methods: &dyn Arity,
        depth: usize,
    ) -> Result<Expr, Error> {
        match operand {
            Operand::Term => self.term(methods, depth),
            Operand::Byte => Ok(Expr::Int(self.le(1)?)),
            Operand::Word => Ok(Expr::Int(self.le(2)?)),
            Operand::DWord => Ok(Expr::Int(self.le(4)?)),
            Operand::Name => Ok(Expr::Name(self.name()?)),
            Operand::Target if self.peek()? == 0x00 => {
                self.at += 1;
                Ok(Expr::Nothing)
            }
            Operand::Super | Operand::Target => self.super_name(methods, depth),
        }
    }

    /// A `SuperName`: a name, a local, an argument, the debug object, or a
    /// reference made by an expression (`RefOf`, `DerefOf`, `Index`, a
    /// method's result).
    pub fn super_name(&mut self, methods: &dyn Arity, depth: usize) -> Result<Expr, Error> {
        // A name is the object it names, or a call if it names a method
        // (which may return a reference).
        self.term(methods, depth)
    }
}
