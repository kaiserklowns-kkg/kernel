//! Running AML (ADR-0153): methods, their control flow and expressions,
//! named data that changes as they run, and the operation regions and
//! fields that reach the hardware through a [`Host`].
//!
//! A [`Machine`] owns a loaded [`Namespace`]. A method's body is read
//! statement by statement when it runs (calls are told from names by the
//! methods every block defined, ADR-0148) and each statement's expression
//! is evaluated at once. Values written to named objects are kept beside
//! the namespace; objects a method defines (`Name`, buffer fields) go when
//! it returns, as ACPI says.
//!
//! The firmware's code is untrusted, and it runs here, in a service:
//! calls nest at most [`MAX_CALLS`] deep, and one evaluation runs at most
//! [`MAX_STEPS`] statements and expressions, so a loop that never ends is
//! an error, not a hang. Integers are 64 bits, or 32 where the DSDT's
//! revision is below 2.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::name::{NameString, Path};
use crate::parse::{self, Expr, Reader};
use crate::{Loaded, Loader, Methods, Namespace, Object};

/// Calls nested at most.
pub const MAX_CALLS: usize = 32;
/// Statements and expressions one evaluation runs at most.
pub const MAX_STEPS: u64 = 4_000_000;

/// Address spaces of operation regions (ACPI 6.5 §19.6.100).
pub mod space {
    pub const SYSTEM_MEMORY: u8 = 0;
    pub const SYSTEM_IO: u8 = 1;
    pub const PCI_CONFIG: u8 = 2;
    pub const EMBEDDED_CONTROL: u8 = 3;
    pub const SMBUS: u8 = 4;
    pub const CMOS: u8 = 5;
}

/// What the interpreter cannot do itself: reach the hardware, wait, tell
/// the time, and hear what the firmware notifies. Accesses are `width`
/// bytes (1, 2, 4 or 8) at `address` in `space`; PCI configuration
/// addresses are `bus << 20 | device << 15 | function << 12 | offset`.
pub trait Host {
    fn read(&mut self, space: u8, address: u64, width: u8) -> core::result::Result<u64, HostError>;
    fn write(
        &mut self,
        space: u8,
        address: u64,
        width: u8,
        value: u64,
    ) -> core::result::Result<(), HostError>;
    /// `Sleep`: at least `ms` milliseconds.
    fn sleep(&mut self, ms: u64) {
        let _ = ms;
    }
    /// `Stall`: at least `us` microseconds, busy.
    fn stall(&mut self, us: u64) {
        let _ = us;
    }
    /// `Timer`: a count of 100 ns units.
    fn timer(&mut self) -> u64 {
        0
    }
    /// `Notify (object, value)`.
    fn notify(&mut self, object: &Path, value: u64) {
        let _ = (object, value);
    }
    /// A value stored to the debug object.
    fn debug(&mut self, value: &Value) {
        let _ = value;
    }
}

/// Why the host could not make an access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostError {
    /// Not allowed (a port or address the system keeps for itself).
    Denied,
    /// No such space or device, or the device did not answer.
    Failed,
}

/// A value AML computes with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Uninitialized,
    Integer(u64),
    /// A string's bytes, without its terminating zero.
    String(Vec<u8>),
    Buffer(Vec<u8>),
    Package(Vec<Value>),
    Reference(Reference),
}

/// What a reference refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reference {
    Named(Path),
    /// A local or an argument of the method running at that depth.
    Local(usize, u8),
    Arg(usize, u8),
    /// An element of a package, buffer or string (`Index`).
    Element(Box<Reference>, usize),
    /// A value that has no name (`Index (Package () {...}, 1)`).
    Temporary(Box<Value>),
    Debug,
}

/// Why running AML failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunError {
    /// A method's body could not be read.
    Parse(parse::Error),
    /// A name that names nothing.
    NotFound(String),
    /// A value of the wrong kind for what was asked of it.
    Type(&'static str),
    /// An index past the end of a package, buffer or string.
    Index(usize),
    /// A region access the host refused or could not make.
    Host(HostError, u8, u64),
    /// An access past the end of its region.
    OutsideRegion(String),
    DivideByZero,
    /// `Fatal` in the firmware's code: type, code, argument.
    Fatal(u8, u32, u64),
    /// Too deep, or too long.
    Limit(&'static str),
}

impl From<parse::Error> for RunError {
    fn from(error: parse::Error) -> Self {
        RunError::Parse(error)
    }
}

type Result<T> = core::result::Result<T, RunError>;

/// How a block of statements ended.
enum Flow {
    Next,
    Break,
    Continue,
    Return(Value),
}

/// A method running: its arguments and locals, and the objects it defined
/// (gone when it returns).
struct Frame {
    args: Vec<Value>,
    locals: Vec<Value>,
    created: Vec<Path>,
}

/// A buffer field: where its bits are.
#[derive(Clone, Debug)]
struct BufferField {
    source: Reference,
    bit: u64,
    bits: u64,
}

/// How a field unit is reached.
enum Unit {
    Region(Path),
    Index {
        index: Path,
        data: Path,
    },
    Bank {
        region: Path,
        bank: Path,
        value: Expr,
    },
}

/// The interfaces `\_OSI` says this system has: those of current Windows,
/// which firmware is written and tested for, and the features ACPI names.
const INTERFACES: [&[u8]; 27] = [
    b"Windows 2000",
    b"Windows 2001",
    b"Windows 2001 SP1",
    b"Windows 2001 SP2",
    b"Windows 2001.1",
    b"Windows 2001.1 SP1",
    b"Windows 2006",
    b"Windows 2006 SP1",
    b"Windows 2006.1",
    b"Windows 2009",
    b"Windows 2012",
    b"Windows 2013",
    b"Windows 2015",
    b"Windows 2016",
    b"Windows 2017",
    b"Windows 2017.2",
    b"Windows 2018",
    b"Windows 2018.2",
    b"Windows 2019",
    b"Windows 2020",
    b"Windows 2021",
    b"Windows 2022",
    b"Module Device",
    b"Processor Device",
    b"3.0 Thermal Model",
    b"Extended Address Space Descriptor",
    b"Processor Aggregator Device",
];

/// A loaded namespace that runs.
pub struct Machine<H: Host> {
    namespace: Namespace,
    host: H,
    frames: Vec<Frame>,
    /// Named data's values once evaluated or written.
    values: BTreeMap<Path, Value>,
    buffer_fields: BTreeMap<Path, BufferField>,
    /// Regions whose offset and length are constants: their space, base
    /// and length, worked out once.
    regions: BTreeMap<Path, (u8, u64, u64)>,
    events: BTreeMap<Path, u64>,
    steps: u64,
    int32: bool,
}

impl<H: Host> Machine<H> {
    pub fn new(namespace: Namespace, host: H) -> Self {
        Self {
            namespace,
            host,
            frames: Vec::new(),
            values: BTreeMap::new(),
            buffer_fields: BTreeMap::new(),
            regions: BTreeMap::new(),
            events: BTreeMap::new(),
            steps: 0,
            int32: false,
        }
    }

    /// Integers of 32 bits: the DSDT's revision is below 2.
    pub fn set_int32(&mut self, int32: bool) {
        self.int32 = int32;
    }

    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    pub fn host(&mut self) -> &mut H {
        &mut self.host
    }

    /// Whether an object is at `path` (one only declared `External` is
    /// not).
    pub fn exists(&self, path: &Path) -> bool {
        matches!(self.namespace.get(path), Some(o) if !matches!(o, Object::External { .. }))
    }

    /// The object at `path` evaluated: a method run with `args`, data's
    /// value, a field read.
    pub fn evaluate(&mut self, path: &Path, args: Vec<Value>) -> Result<Value> {
        self.steps = 0;
        let value = self.call(path, args)?;
        self.plain(value)
    }

    /// As [`evaluate`](Self::evaluate), an integer.
    pub fn evaluate_int(&mut self, path: &Path, args: Vec<Value>) -> Result<u64> {
        let value = self.evaluate(path, args)?;
        self.to_int(&value)
    }

    /// A device's status (`_STA`): its bits, or all set if it has none.
    pub fn status(&mut self, device: &Path) -> Result<u64> {
        let sta = device.child(*b"_STA");
        if self.exists(&sta) {
            self.evaluate_int(&sta, Vec::new())
        } else {
            Ok(0x0f)
        }
    }

    /// A device's hardware ID (`_HID`), constant or computed: an EISA ID
    /// as text, or a string.
    pub fn hardware_id(&mut self, device: &Path) -> Option<String> {
        let hid = device.child(*b"_HID");
        if !self.exists(&hid) {
            return None;
        }
        match self.evaluate(&hid, Vec::new()).ok()? {
            Value::Integer(v) => {
                Some(String::from_utf8_lossy(&crate::name::eisa_id(v as u32)).into_owned())
            }
            Value::String(text) => Some(String::from_utf8_lossy(&text).into_owned()),
            _ => None,
        }
    }

    /// Tells the firmware a space's handler is ready: `_REG (space, 1)` for
    /// each device that has a region in it (ACPI 6.5 §6.5.4). Returns how
    /// many ran, and the first error of those that failed.
    pub fn connect(&mut self, region_space: u8) -> (usize, Option<RunError>) {
        let mut scopes: Vec<Path> = self
            .namespace
            .iter()
            .filter(|(_, o)| matches!(o, Object::Region { space, .. } if *space == region_space))
            .map(|(p, _)| p.parent())
            .collect();
        scopes.dedup();
        let (mut ran, mut first) = (0, None);
        for scope in scopes {
            let reg = scope.child(*b"_REG");
            if !self.exists(&reg) {
                continue;
            }
            match self.evaluate(
                &reg,
                vec![Value::Integer(u64::from(region_space)), Value::Integer(1)],
            ) {
                Ok(_) => ran += 1,
                Err(error) => {
                    first.get_or_insert(error);
                }
            }
        }
        (ran, first)
    }

    // ---- Calls --------------------------------------------------------------

    fn call(&mut self, path: &Path, args: Vec<Value>) -> Result<Value> {
        match self.namespace.get(path).cloned() {
            Some(Object::Builtin { .. }) => {
                // `\_OSI (interface)`.
                let asked = match args.first() {
                    Some(Value::String(text)) => text.clone(),
                    _ => Vec::new(),
                };
                let has = INTERFACES.contains(&asked.as_slice());
                Ok(Value::Integer(if has { self.ones() } else { 0 }))
            }
            Some(Object::Method { body, .. }) => {
                if self.frames.len() >= MAX_CALLS {
                    return Err(RunError::Limit("calls nested too deep"));
                }
                let mut args = args;
                args.resize(7, Value::Uninitialized);
                self.frames.push(Frame {
                    args,
                    locals: vec![Value::Uninitialized; 8],
                    created: Vec::new(),
                });
                let flow = self.block(&body, 0, path);
                let frame = self.frames.pop().expect("pushed above");
                for created in frame.created {
                    self.namespace.remove(&created);
                    self.values.remove(&created);
                    self.buffer_fields.remove(&created);
                }
                match flow? {
                    Flow::Return(value) => Ok(value),
                    _ => Ok(Value::Integer(0)),
                }
            }
            Some(_) => self.read_named(path),
            None => Err(RunError::NotFound(format!("{path}"))),
        }
    }

    fn tick(&mut self) -> Result<()> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return Err(RunError::Limit("ran too long"));
        }
        Ok(())
    }

    fn depth(&self) -> usize {
        self.frames.len().saturating_sub(1)
    }

    fn frame(&mut self) -> Result<&mut Frame> {
        self.frames
            .last_mut()
            .ok_or(RunError::Type("a local or argument outside a method"))
    }

    // ---- Statements -----------------------------------------------------------

    /// The statements of `code` from `start` to its end, in `scope`.
    fn block(&mut self, code: &[u8], start: usize, scope: &Path) -> Result<Flow> {
        let mut r = Reader {
            bytes: code,
            at: start,
        };
        while !r.done() {
            match self.statement(&mut r, scope)? {
                Flow::Next => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Next)
    }

    fn expr(&self, r: &mut Reader<'_>, scope: &Path) -> Result<Expr> {
        let methods = Methods {
            namespace: &self.namespace,
            scope,
        };
        Ok(r.term(&methods, 0)?)
    }

    fn statement(&mut self, r: &mut Reader<'_>, scope: &Path) -> Result<Flow> {
        self.tick()?;
        let len = r.bytes.len();
        let first = r.peek()?;
        let second = r.bytes.get(r.at + 1).copied().unwrap_or(0);
        match (first, second) {
            (0xa0, _) => {
                // If (predicate) {...} [Else {...}]
                r.at += 1;
                let end = r.pkg_length(len)?;
                let predicate = self.expr(r, scope)?;
                let taken = self.eval_int(&predicate, scope)? != 0;
                let mut flow = if taken {
                    self.block(&r.bytes[..end], r.at, scope)?
                } else {
                    Flow::Next
                };
                r.at = end;
                if r.peek().ok() == Some(0xa1) {
                    r.at += 1;
                    let else_end = r.pkg_length(len)?;
                    if !taken {
                        flow = self.block(&r.bytes[..else_end], r.at, scope)?;
                    }
                    r.at = else_end;
                }
                Ok(flow)
            }
            (0xa1, _) => {
                // An Else without its If: passed over.
                r.at += 1;
                r.at = r.pkg_length(len)?;
                Ok(Flow::Next)
            }
            (0xa2, _) => {
                // While (predicate) {...}
                r.at += 1;
                let end = r.pkg_length(len)?;
                let condition = r.at;
                let flow = loop {
                    self.tick()?;
                    r.at = condition;
                    let predicate = self.expr(r, scope)?;
                    if self.eval_int(&predicate, scope)? == 0 {
                        break Flow::Next;
                    }
                    match self.block(&r.bytes[..end], r.at, scope)? {
                        Flow::Break => break Flow::Next,
                        Flow::Return(value) => break Flow::Return(value),
                        Flow::Next | Flow::Continue => {}
                    }
                };
                r.at = end;
                Ok(flow)
            }
            (0xa4, _) => {
                r.at += 1;
                let value = self.expr(r, scope)?;
                let value = self.eval(&value, scope)?;
                Ok(Flow::Return(value))
            }
            (0xa5, _) => {
                r.at += 1;
                Ok(Flow::Break)
            }
            (0x9f, _) => {
                r.at += 1;
                Ok(Flow::Continue)
            }
            (0xa3 | 0xcc, _) => {
                r.at += 1;
                Ok(Flow::Next)
            }
            (0x14, _) => {
                // A method inside a method: not allowed; passed over.
                r.at += 1;
                r.at = r.pkg_length(len)?;
                Ok(Flow::Next)
            }
            (0x08, _) => {
                // Name (X, value): the method's own, gone when it returns.
                r.at += 1;
                let name = r.name()?;
                let path = name
                    .resolve(scope)
                    .filter(|_| !name.segs.is_empty())
                    .ok_or(RunError::Type("a name to define"))?;
                let value = self.expr(r, scope)?;
                let value = self.eval(&value, scope)?;
                self.define_own(path.clone(), Object::Data(Expr::Int(0)));
                self.values.insert(path, value);
                Ok(Flow::Next)
            }
            (0x06 | 0x10 | 0x15, _) | (0x5b, 0x01 | 0x02 | 0x80..=0x88) => {
                // Other definitions inside a method (regions, fields,
                // mutexes): defined as a block defines them.
                let mut loader = Loader {
                    namespace: &mut self.namespace,
                    loaded: Loaded::default(),
                };
                loader.term(r, scope, len, 0)?;
                Ok(Flow::Next)
            }
            _ => {
                let expr = self.expr(r, scope)?;
                self.eval(&expr, scope)?;
                Ok(Flow::Next)
            }
        }
    }

    /// An object the running method defines: gone when it returns.
    fn define_own(&mut self, path: Path, object: Object) {
        if self.namespace.get(&path).is_none()
            && let Some(frame) = self.frames.last_mut()
        {
            frame.created.push(path.clone());
        }
        self.namespace.insert(path, object);
    }

    // ---- Expressions ----------------------------------------------------------

    fn ones(&self) -> u64 {
        if self.int32 { 0xffff_ffff } else { u64::MAX }
    }

    fn int(&self, v: u64) -> Value {
        Value::Integer(v & self.ones())
    }

    fn lookup(&self, name: &NameString, scope: &Path) -> Result<Path> {
        self.namespace
            .lookup(name, scope)
            .ok_or_else(|| RunError::NotFound(format!("{name}")))
    }

    fn eval_int(&mut self, e: &Expr, scope: &Path) -> Result<u64> {
        let v = self.eval(e, scope)?;
        self.to_int(&v)
    }

    fn eval(&mut self, e: &Expr, scope: &Path) -> Result<Value> {
        self.tick()?;
        match e {
            Expr::Int(v) => Ok(self.int(*v)),
            Expr::Str(text) => Ok(Value::String(text.clone())),
            Expr::Buffer(size, init) => {
                let size = self.eval_int(size, scope)? as usize;
                if size > 1 << 20 {
                    return Err(RunError::Limit("a buffer too large"));
                }
                let mut bytes = init.clone();
                if bytes.len() < size {
                    bytes.resize(size, 0);
                }
                Ok(Value::Buffer(bytes))
            }
            Expr::Package(count, elements) => {
                let count = self.eval_int(count, scope)? as usize;
                if count > 1 << 16 {
                    return Err(RunError::Limit("a package too large"));
                }
                let mut values = Vec::with_capacity(count.max(elements.len()));
                for element in elements {
                    let value = match element {
                        // A name in a package refers to its object.
                        Expr::Name(name) => match self.namespace.lookup(name, scope) {
                            Some(path) => Value::Reference(Reference::Named(path)),
                            None => Value::String(format!("{name}").into_bytes()),
                        },
                        other => self.eval(other, scope)?,
                    };
                    values.push(value);
                }
                if values.len() < count {
                    values.resize(count, Value::Uninitialized);
                }
                Ok(Value::Package(values))
            }
            Expr::Name(name) => {
                let path = self.lookup(name, scope)?;
                self.read_named(&path)
            }
            Expr::Call(name, args) => {
                let path = self.lookup(name, scope)?;
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(self.eval_arg(arg, scope)?);
                }
                self.call(&path, values)
            }
            Expr::Local(i) => Ok(self.frame()?.locals[usize::from(*i) & 7].clone()),
            Expr::Arg(i) => {
                let value = self.frame()?.args[usize::from(*i).min(6)].clone();
                // An argument that is a reference is followed.
                match value {
                    Value::Reference(r) => self.read_ref(&r),
                    other => Ok(other),
                }
            }
            Expr::Debug => Ok(Value::Reference(Reference::Debug)),
            Expr::Nothing => Ok(Value::Uninitialized),
            Expr::Op(code, ops) => self.op(*code, ops, scope),
        }
    }

    /// An argument of a call: a device or other object by reference, the
    /// rest by value.
    fn eval_arg(&mut self, e: &Expr, scope: &Path) -> Result<Value> {
        if let Expr::Name(name) = e {
            let path = self.lookup(name, scope)?;
            return match self.namespace.get(&path) {
                Some(Object::Data(_) | Object::Field { .. } | Object::IndexField { .. })
                | Some(Object::BankField { .. } | Object::BufferField) => self.read_named(&path),
                _ => Ok(Value::Reference(Reference::Named(path))),
            };
        }
        self.eval(e, scope)
    }

    /// The reference a `SuperName` makes.
    fn reference_of(&mut self, e: &Expr, scope: &Path) -> Result<Reference> {
        let depth = self.depth();
        Ok(match e {
            Expr::Name(name) => Reference::Named(self.lookup(name, scope)?),
            Expr::Local(i) => Reference::Local(depth, *i & 7),
            Expr::Arg(i) => match &self.frame()?.args[usize::from(*i).min(6)] {
                Value::Reference(r) => r.clone(),
                _ => Reference::Arg(depth, (*i).min(6)),
            },
            Expr::Debug => Reference::Debug,
            other => match self.eval(other, scope)? {
                Value::Reference(r) => r,
                value => Reference::Temporary(Box::new(value)),
            },
        })
    }

    fn op(&mut self, code: u16, ops: &[Expr], scope: &Path) -> Result<Value> {
        let arg = |i: usize| ops.get(i).ok_or(RunError::Type("an operand"));
        match code {
            // Store (source, target)
            0x70 => {
                let value = self.eval(arg(0)?, scope)?;
                let value = self.plain(value)?;
                self.store(value.clone(), arg(1)?, scope)?;
                Ok(value)
            }
            // RefOf
            0x71 => Ok(Value::Reference(self.reference_of(arg(0)?, scope)?)),
            // Add, Subtract, Multiply, ShiftLeft/Right, And, NAnd, Or, NOr, Xor, Mod
            0x72 | 0x74 | 0x77 | 0x79..=0x7f | 0x85 => {
                let a = self.eval_int(arg(0)?, scope)?;
                let b = self.eval_int(arg(1)?, scope)?;
                let v = match code {
                    0x72 => a.wrapping_add(b),
                    0x74 => a.wrapping_sub(b),
                    0x77 => a.wrapping_mul(b),
                    0x79 => {
                        if b >= 64 {
                            0
                        } else {
                            a << b
                        }
                    }
                    0x7a => {
                        if b >= 64 {
                            0
                        } else {
                            a >> b
                        }
                    }
                    0x7b => a & b,
                    0x7c => !(a & b),
                    0x7d => a | b,
                    0x7e => !(a | b),
                    0x7f => a ^ b,
                    _ => a.checked_rem(b).ok_or(RunError::DivideByZero)?,
                };
                let v = self.int(v);
                self.store(v.clone(), arg(2)?, scope)?;
                Ok(v)
            }
            // Concatenate, ConcatenateResTemplate
            0x73 | 0x84 => {
                let a = self.eval(arg(0)?, scope)?;
                let b = self.eval(arg(1)?, scope)?;
                let v = if code == 0x84 {
                    let mut out = self.buffer_of(&a)?;
                    strip_end_tag(&mut out);
                    let mut tail = self.buffer_of(&b)?;
                    strip_end_tag(&mut tail);
                    out.extend_from_slice(&tail);
                    out.extend_from_slice(&[0x79, 0x00]);
                    Value::Buffer(out)
                } else {
                    match a {
                        Value::Integer(x) => {
                            let mut out = self.int_bytes(x);
                            let y = self.to_int(&b)?;
                            out.extend_from_slice(&self.int_bytes(y));
                            Value::Buffer(out)
                        }
                        Value::Buffer(mut x) => {
                            x.extend_from_slice(&self.buffer_of(&b)?);
                            Value::Buffer(x)
                        }
                        other => {
                            let mut x = self.string_of(&other)?;
                            x.extend_from_slice(&self.string_of(&b)?);
                            Value::String(x)
                        }
                    }
                };
                self.store(v.clone(), arg(2)?, scope)?;
                Ok(v)
            }
            // Increment, Decrement
            0x75 | 0x76 => {
                let v = self.eval_int(arg(0)?, scope)?;
                let v = self.int(if code == 0x75 {
                    v.wrapping_add(1)
                } else {
                    v.wrapping_sub(1)
                });
                self.store(v.clone(), arg(0)?, scope)?;
                Ok(v)
            }
            // Divide (dividend, divisor, remainder, result)
            0x78 => {
                let a = self.eval_int(arg(0)?, scope)?;
                let b = self.eval_int(arg(1)?, scope)?;
                if b == 0 {
                    return Err(RunError::DivideByZero);
                }
                self.store(self.int(a % b), arg(2)?, scope)?;
                let q = self.int(a / b);
                self.store(q.clone(), arg(3)?, scope)?;
                Ok(q)
            }
            // Not, FindSetLeftBit, FindSetRightBit
            0x80..=0x82 => {
                let a = self.eval_int(arg(0)?, scope)?;
                let v = match code {
                    0x80 => !a,
                    0x81 => {
                        if a == 0 {
                            0
                        } else {
                            64 - u64::from(a.leading_zeros())
                        }
                    }
                    _ => {
                        if a == 0 {
                            0
                        } else {
                            u64::from(a.trailing_zeros()) + 1
                        }
                    }
                };
                let v = self.int(v);
                self.store(v.clone(), arg(1)?, scope)?;
                Ok(v)
            }
            // DerefOf
            0x83 => match self.eval(arg(0)?, scope)? {
                Value::Reference(r) => self.read_ref(&r),
                Value::String(path) => {
                    let text = String::from_utf8_lossy(&path).into_owned();
                    let name = parse_name(&text).ok_or(RunError::NotFound(text))?;
                    let path = self.lookup(&name, scope)?;
                    self.read_named(&path)
                }
                _ => Err(RunError::Type("DerefOf of no reference")),
            },
            // Notify (object, value)
            0x86 => {
                let object = self.reference_of(arg(0)?, scope)?;
                let value = self.eval_int(arg(1)?, scope)?;
                if let Reference::Named(path) = object {
                    self.host.notify(&path, value);
                }
                Ok(Value::Integer(0))
            }
            // SizeOf
            0x87 => {
                let value = self.eval(arg(0)?, scope)?;
                let value = self.plain(value)?;
                Ok(Value::Integer(match value {
                    Value::String(s) => s.len(),
                    Value::Buffer(b) => b.len(),
                    Value::Package(p) => p.len(),
                    _ => return Err(RunError::Type("SizeOf of no string, buffer or package")),
                } as u64))
            }
            // Index (source, index, target)
            0x88 => {
                let base = self.reference_of(arg(0)?, scope)?;
                let index = self.eval_int(arg(1)?, scope)? as usize;
                let r = Value::Reference(Reference::Element(Box::new(base), index));
                self.store(r.clone(), arg(2)?, scope)?;
                Ok(r)
            }
            // Match (package, op, value, op, value, start)
            0x89 => {
                let package = self.eval(arg(0)?, scope)?;
                let Value::Package(items) = self.plain(package)? else {
                    return Err(RunError::Type("Match of no package"));
                };
                let op1 = self.eval_int(arg(1)?, scope)?;
                let v1 = self.eval_int(arg(2)?, scope)?;
                let op2 = self.eval_int(arg(3)?, scope)?;
                let v2 = self.eval_int(arg(4)?, scope)?;
                let start = self.eval_int(arg(5)?, scope)? as usize;
                let test = |op: u64, x: u64, v: u64| match op {
                    0 => true,
                    1 => x == v,
                    2 => x <= v,
                    3 => x < v,
                    4 => x >= v,
                    5 => x > v,
                    _ => false,
                };
                for (i, item) in items.iter().enumerate().skip(start) {
                    let Value::Integer(x) = item else {
                        continue;
                    };
                    if test(op1, *x, v1) && test(op2, *x, v2) {
                        return Ok(Value::Integer(i as u64));
                    }
                }
                Ok(Value::Integer(self.ones()))
            }
            // CreateDWord/Word/Byte/Bit/QWordField (source, index, name)
            0x8a..=0x8d | 0x8f => {
                let source = self.reference_of(arg(0)?, scope)?;
                let index = self.eval_int(arg(1)?, scope)?;
                let (bit, bits) = match code {
                    0x8a => (index.saturating_mul(8), 32),
                    0x8b => (index.saturating_mul(8), 16),
                    0x8c => (index.saturating_mul(8), 8),
                    0x8d => (index, 1),
                    _ => (index.saturating_mul(8), 64),
                };
                self.create_field(source, bit, bits, arg(2)?, scope)?;
                Ok(Value::Integer(0))
            }
            // CreateField (source, bit index, bits, name)
            0x5b13 => {
                let source = self.reference_of(arg(0)?, scope)?;
                let bit = self.eval_int(arg(1)?, scope)?;
                let bits = self.eval_int(arg(2)?, scope)?;
                self.create_field(source, bit, bits, arg(3)?, scope)?;
                Ok(Value::Integer(0))
            }
            // ObjectType
            0x8e => Ok(Value::Integer(self.object_type(arg(0)?, scope)?)),
            // LAnd, LOr
            0x90 | 0x91 => {
                let a = self.eval_int(arg(0)?, scope)? != 0;
                let b = self.eval_int(arg(1)?, scope)? != 0;
                let v = if code == 0x90 { a && b } else { a || b };
                Ok(self.boolean(v))
            }
            // LNot
            0x92 => {
                let a = self.eval_int(arg(0)?, scope)?;
                Ok(self.boolean(a == 0))
            }
            // LEqual, LGreater, LLess
            0x93..=0x95 => {
                let a = self.eval(arg(0)?, scope)?;
                let b = self.eval(arg(1)?, scope)?;
                let ordering = self.compare(&a, &b)?;
                Ok(self.boolean(match code {
                    0x93 => ordering.is_eq(),
                    0x94 => ordering.is_gt(),
                    _ => ordering.is_lt(),
                }))
            }
            // ToBuffer, ToDecimalString, ToHexString, ToInteger
            0x96..=0x99 => {
                let a = self.eval(arg(0)?, scope)?;
                let v = match code {
                    0x96 => Value::Buffer(self.buffer_of(&a)?),
                    0x97 => Value::String(self.decimal_string(&a)?),
                    0x98 => Value::String(self.hex_string(&a)?),
                    _ => Value::Integer(self.explicit_int(&a)?),
                };
                self.store(v.clone(), arg(1)?, scope)?;
                Ok(v)
            }
            // ToString (buffer, length, target)
            0x9c => {
                let a = self.eval(arg(0)?, scope)?;
                let bytes = self.buffer_of(&a)?;
                let limit = self.eval_int(arg(1)?, scope)?;
                let end = bytes
                    .iter()
                    .position(|&b| b == 0)
                    .unwrap_or(bytes.len())
                    .min(usize::try_from(limit).unwrap_or(usize::MAX));
                let v = Value::String(bytes[..end].to_vec());
                self.store(v.clone(), arg(2)?, scope)?;
                Ok(v)
            }
            // CopyObject (source, target): no conversion.
            0x9d => {
                let value = self.eval(arg(0)?, scope)?;
                match arg(1)? {
                    Expr::Name(name) => {
                        let path = self.lookup(name, scope)?;
                        self.values.insert(path, value.clone());
                    }
                    target => self.store(value.clone(), target, scope)?,
                }
                Ok(value)
            }
            // Mid (source, index, length, target)
            0x9e => {
                let source = self.eval(arg(0)?, scope)?;
                let index = self.eval_int(arg(1)?, scope)? as usize;
                let length = self.eval_int(arg(2)?, scope)? as usize;
                let cut = |bytes: &[u8]| {
                    let start = index.min(bytes.len());
                    let end = start.saturating_add(length).min(bytes.len());
                    bytes[start..end].to_vec()
                };
                let v = match self.plain(source)? {
                    Value::Buffer(b) => Value::Buffer(cut(&b)),
                    other => Value::String(cut(&self.string_of(&other)?)),
                };
                self.store(v.clone(), arg(3)?, scope)?;
                Ok(v)
            }
            // Return, Break, Continue, Noop as expressions: nothing.
            0x9f | 0xa3 | 0xa5 | 0xcc => Ok(Value::Integer(0)),
            0xa4 => self.eval(arg(0)?, scope),
            // CondRefOf (name, target)
            0x5b12 => {
                let found = match arg(0)? {
                    Expr::Name(name) => self
                        .namespace
                        .lookup(name, scope)
                        .filter(|p| self.exists(p))
                        .map(Reference::Named),
                    other => Some(self.reference_of(other, scope)?),
                };
                match found {
                    Some(r) => {
                        self.store(Value::Reference(r), arg(1)?, scope)?;
                        Ok(Value::Integer(self.ones()))
                    }
                    None => Ok(Value::Integer(0)),
                }
            }
            // Load, LoadTable: tables loaded at run time are not supported;
            // the firmware is told it failed.
            0x5b1f | 0x5b20 => Ok(Value::Integer(0)),
            // Stall (µs), Sleep (ms)
            0x5b21 => {
                let us = self.eval_int(arg(0)?, scope)?;
                self.host.stall(us.min(100));
                Ok(Value::Integer(0))
            }
            0x5b22 => {
                let ms = self.eval_int(arg(0)?, scope)?;
                self.host.sleep(ms.min(5_000));
                Ok(Value::Integer(0))
            }
            // Acquire (mutex, timeout): one thread runs AML; it is had.
            0x5b23 => Ok(Value::Integer(0)),
            // Signal, Reset, Release, Unload
            0x5b24 | 0x5b26 => {
                if let Reference::Named(path) = self.reference_of(arg(0)?, scope)? {
                    let count = self.events.entry(path).or_insert(0);
                    *count = if code == 0x5b24 {
                        count.saturating_add(1)
                    } else {
                        0
                    };
                }
                Ok(Value::Integer(0))
            }
            0x5b27 | 0x5b2a => Ok(Value::Integer(0)),
            // Wait (event, timeout): signalled already, or it times out.
            0x5b25 => {
                let event = self.reference_of(arg(0)?, scope)?;
                let _ = self.eval_int(arg(1)?, scope)?;
                let signalled = match event {
                    Reference::Named(path) => match self.events.get_mut(&path) {
                        Some(count) if *count > 0 => {
                            *count -= 1;
                            true
                        }
                        _ => false,
                    },
                    _ => false,
                };
                Ok(Value::Integer(if signalled { 0 } else { self.ones() }))
            }
            // FromBCD, ToBCD
            0x5b28 | 0x5b29 => {
                let a = self.eval_int(arg(0)?, scope)?;
                let v = if code == 0x5b28 {
                    from_bcd(a)
                } else {
                    to_bcd(a)
                };
                let v = self.int(v);
                self.store(v.clone(), arg(1)?, scope)?;
                Ok(v)
            }
            // Revision: the interpreter's.
            0x5b30 => Ok(Value::Integer(1)),
            // Fatal (type, code, argument)
            0x5b32 => {
                let kind = self.eval_int(arg(0)?, scope)? as u8;
                let fatal = self.eval_int(arg(1)?, scope)? as u32;
                let argument = self.eval_int(arg(2)?, scope)?;
                Err(RunError::Fatal(kind, fatal, argument))
            }
            // Timer
            0x5b33 => Ok(Value::Integer(self.host.timer())),
            _ => Err(RunError::Type("an opcode the interpreter does not run")),
        }
    }

    fn boolean(&self, v: bool) -> Value {
        Value::Integer(if v { self.ones() } else { 0 })
    }

    fn create_field(
        &mut self,
        source: Reference,
        bit: u64,
        bits: u64,
        name: &Expr,
        scope: &Path,
    ) -> Result<()> {
        let Expr::Name(name) = name else {
            return Err(RunError::Type("a field's name"));
        };
        let path = name
            .resolve(scope)
            .filter(|_| !name.segs.is_empty())
            .ok_or(RunError::Type("a field's name"))?;
        if bits == 0 || bits > 1 << 16 || bit > 1 << 32 {
            return Err(RunError::Type("a field's size or place"));
        }
        // Inside a method, its own; at load time, kept.
        if self.frames.is_empty() {
            self.namespace.insert(path.clone(), Object::BufferField);
        } else {
            self.define_own(path.clone(), Object::BufferField);
        }
        self.buffer_fields
            .insert(path, BufferField { source, bit, bits });
        Ok(())
    }

    fn object_type(&mut self, e: &Expr, scope: &Path) -> Result<u64> {
        if let Expr::Name(name) = e {
            let path = self.lookup(name, scope)?;
            return Ok(match self.namespace.get(&path) {
                Some(Object::Data(_)) => return self.value_type(&Expr::Name(name.clone()), scope),
                Some(
                    Object::Field { .. } | Object::IndexField { .. } | Object::BankField { .. },
                ) => 5,
                Some(Object::Device) => 6,
                Some(Object::Event) => 7,
                Some(Object::Method { .. } | Object::Builtin { .. }) => 8,
                Some(Object::Mutex { .. }) => 9,
                Some(Object::Region { .. }) => 10,
                Some(Object::PowerResource) => 11,
                Some(Object::Processor { .. }) => 12,
                Some(Object::ThermalZone) => 13,
                Some(Object::BufferField) => 14,
                _ => 0,
            });
        }
        self.value_type(e, scope)
    }

    fn value_type(&mut self, e: &Expr, scope: &Path) -> Result<u64> {
        let value = self.eval(e, scope)?;
        Ok(match self.plain(value)? {
            Value::Uninitialized => 0,
            Value::Integer(_) => 1,
            Value::String(_) => 2,
            Value::Buffer(_) => 3,
            Value::Package(_) => 4,
            Value::Reference(_) => 16,
        })
    }

    // ---- Named objects and references -------------------------------------

    fn read_named(&mut self, path: &Path) -> Result<Value> {
        if let Some(value) = self.values.get(path) {
            return Ok(value.clone());
        }
        match self.namespace.get(path).cloned() {
            Some(Object::Data(expr)) => {
                let value = self.eval(&expr, &path.parent())?;
                self.values.insert(path.clone(), value.clone());
                Ok(value)
            }
            Some(Object::Field { .. } | Object::IndexField { .. } | Object::BankField { .. }) => {
                self.read_field(path)
            }
            Some(Object::BufferField) => self.read_buffer_field(path),
            Some(Object::Alias(target)) => self.read_named(&target),
            Some(Object::Method { .. } | Object::Builtin { .. }) => self.call(path, Vec::new()),
            Some(Object::External { .. }) | None => Err(RunError::NotFound(format!("{path}"))),
            Some(_) => Ok(Value::Reference(Reference::Named(path.clone()))),
        }
    }

    fn write_named(&mut self, path: &Path, value: Value) -> Result<()> {
        match self.namespace.get(path).cloned() {
            Some(Object::Field { .. } | Object::IndexField { .. } | Object::BankField { .. }) => {
                self.write_field(path, &value)
            }
            Some(Object::BufferField) => self.write_buffer_field(path, &value),
            Some(Object::Alias(target)) => self.write_named(&target, value),
            Some(Object::Data(_)) => {
                let now = self.read_named(path)?;
                let converted = self.convert_like(&now, value)?;
                self.values.insert(path.clone(), converted);
                Ok(())
            }
            Some(Object::External { .. }) | None => Err(RunError::NotFound(format!("{path}"))),
            Some(_) => Err(RunError::Type("a store to an object that holds no value")),
        }
    }

    /// `value` as stored into an object holding `now` (ACPI 6.5 §19.3.5):
    /// an integer stays an integer, a string a string, a buffer keeps its
    /// length.
    fn convert_like(&mut self, now: &Value, value: Value) -> Result<Value> {
        Ok(match now {
            Value::Integer(_) => Value::Integer(self.to_int(&value)?),
            Value::String(_) => Value::String(self.string_of(&value)?),
            Value::Buffer(old) if !old.is_empty() => {
                let new = self.buffer_of(&value)?;
                let mut out = vec![0u8; old.len()];
                let n = out.len().min(new.len());
                out[..n].copy_from_slice(&new[..n]);
                Value::Buffer(out)
            }
            _ => value,
        })
    }

    /// Stores `value` where `target` says (`Nothing` drops it).
    fn store(&mut self, value: Value, target: &Expr, scope: &Path) -> Result<()> {
        match target {
            Expr::Nothing => Ok(()),
            Expr::Int(0) => Ok(()),
            Expr::Debug => {
                self.host.debug(&value);
                Ok(())
            }
            Expr::Local(i) => {
                self.frame()?.locals[usize::from(*i) & 7] = value;
                Ok(())
            }
            Expr::Arg(i) => {
                let slot = usize::from(*i).min(6);
                match self.frame()?.args[slot].clone() {
                    Value::Reference(r) => self.write_ref(&r, value),
                    _ => {
                        self.frame()?.args[slot] = value;
                        Ok(())
                    }
                }
            }
            Expr::Name(name) => {
                let path = self.lookup(name, scope)?;
                self.write_named(&path, value)
            }
            other => match self.eval(other, scope)? {
                Value::Reference(r) => self.write_ref(&r, value),
                _ => Err(RunError::Type("a store to no reference")),
            },
        }
    }

    fn read_ref(&mut self, r: &Reference) -> Result<Value> {
        match r {
            Reference::Named(path) => self.read_named(path),
            Reference::Local(depth, i) => self.slot(*depth, false, *i).map(|v| v.clone()),
            Reference::Arg(depth, i) => self.slot(*depth, true, *i).map(|v| v.clone()),
            Reference::Temporary(value) => Ok((**value).clone()),
            Reference::Debug => Ok(Value::Uninitialized),
            Reference::Element(base, index) => {
                let container = self.read_ref(base)?;
                match self.plain(container)? {
                    Value::Package(items) => {
                        items.get(*index).cloned().ok_or(RunError::Index(*index))
                    }
                    Value::Buffer(bytes) | Value::String(bytes) => bytes
                        .get(*index)
                        .map(|&b| Value::Integer(u64::from(b)))
                        .ok_or(RunError::Index(*index)),
                    _ => Err(RunError::Type("an index into no package or buffer")),
                }
            }
        }
    }

    fn write_ref(&mut self, r: &Reference, value: Value) -> Result<()> {
        match r {
            Reference::Named(path) => self.write_named(path, value),
            Reference::Local(depth, i) => {
                *self.slot(*depth, false, *i)? = value;
                Ok(())
            }
            Reference::Arg(depth, i) => {
                *self.slot(*depth, true, *i)? = value;
                Ok(())
            }
            Reference::Debug => {
                self.host.debug(&value);
                Ok(())
            }
            Reference::Temporary(_) => Ok(()),
            Reference::Element(base, index) => {
                let byte = match &value {
                    Value::Integer(v) => Some(*v as u8),
                    _ => None,
                };
                self.materialize(base)?;
                let container = self.container(base)?;
                match container {
                    Value::Package(items) => {
                        *items.get_mut(*index).ok_or(RunError::Index(*index))? = value;
                        Ok(())
                    }
                    Value::Buffer(bytes) | Value::String(bytes) => {
                        let slot = bytes.get_mut(*index).ok_or(RunError::Index(*index))?;
                        *slot = byte.ok_or(RunError::Type("a byte"))?;
                        Ok(())
                    }
                    _ => Err(RunError::Type("an index into no package or buffer")),
                }
            }
        }
    }

    /// Makes sure a named container's value is kept beside the namespace,
    /// so that writing into it lasts.
    fn materialize(&mut self, r: &Reference) -> Result<()> {
        match r {
            Reference::Named(path) => {
                if !self.values.contains_key(path) {
                    let value = self.read_named(path)?;
                    self.values.insert(path.clone(), value);
                }
                Ok(())
            }
            Reference::Element(base, _) => self.materialize(base),
            _ => Ok(()),
        }
    }

    fn container(&mut self, r: &Reference) -> Result<&mut Value> {
        match r {
            Reference::Named(path) => self
                .values
                .get_mut(path)
                .ok_or(RunError::Type("an object that holds no value")),
            Reference::Local(depth, i) => self.slot(*depth, false, *i),
            Reference::Arg(depth, i) => self.slot(*depth, true, *i),
            Reference::Element(base, index) => match self.container(base)? {
                Value::Package(items) => items.get_mut(*index).ok_or(RunError::Index(*index)),
                _ => Err(RunError::Type("an index into no package")),
            },
            Reference::Temporary(_) | Reference::Debug => {
                Err(RunError::Type("a store into a temporary"))
            }
        }
    }

    fn slot(&mut self, depth: usize, arg: bool, i: u8) -> Result<&mut Value> {
        let frame = self
            .frames
            .get_mut(depth)
            .ok_or(RunError::Type("a reference to a method that returned"))?;
        let slots = if arg {
            &mut frame.args
        } else {
            &mut frame.locals
        };
        slots
            .get_mut(usize::from(i))
            .ok_or(RunError::Type("a local or argument"))
    }

    /// A value with its references followed (what a method returns, what
    /// `SizeOf` measures).
    fn plain(&mut self, value: Value) -> Result<Value> {
        match value {
            Value::Reference(Reference::Named(path))
                if matches!(
                    self.namespace.get(&path),
                    Some(Object::Data(_) | Object::Field { .. } | Object::IndexField { .. })
                        | Some(Object::BankField { .. } | Object::BufferField)
                ) =>
            {
                self.read_named(&path)
            }
            Value::Reference(
                r @ (Reference::Local(..)
                | Reference::Arg(..)
                | Reference::Element(..)
                | Reference::Temporary(_)),
            ) => self.read_ref(&r),
            other => Ok(other),
        }
    }

    // ---- Conversions ----------------------------------------------------------

    fn int_bytes(&self, v: u64) -> Vec<u8> {
        let width = if self.int32 { 4 } else { 8 };
        v.to_le_bytes()[..width].to_vec()
    }

    /// The integer a value is (ACPI 6.5 §19.3.5.5): a buffer's first bytes,
    /// a string's hexadecimal digits.
    pub fn to_int(&mut self, value: &Value) -> Result<u64> {
        Ok(match value {
            Value::Integer(v) => *v,
            Value::Uninitialized => 0,
            Value::Buffer(bytes) => {
                let width = if self.int32 { 4 } else { 8 };
                bytes
                    .iter()
                    .take(width)
                    .rev()
                    .fold(0u64, |v, &b| (v << 8) | u64::from(b))
            }
            Value::String(text) => parse_int(text, 16),
            Value::Reference(r) => {
                let v = self.read_ref(r)?;
                if matches!(v, Value::Reference(_)) {
                    return Err(RunError::Type("an integer"));
                }
                return self.to_int(&v);
            }
            Value::Package(_) => return Err(RunError::Type("an integer, not a package")),
        } & self.ones())
    }

    /// `ToInteger`: a string in decimal, or hexadecimal after `0x`.
    fn explicit_int(&mut self, value: &Value) -> Result<u64> {
        match value {
            Value::String(text) => {
                let (digits, radix) = match text.strip_prefix(b"0x").or(text.strip_prefix(b"0X")) {
                    Some(rest) => (rest, 16),
                    None => (text.as_slice(), 10),
                };
                Ok(parse_int(digits, radix) & self.ones())
            }
            other => self.to_int(other),
        }
    }

    fn buffer_of(&mut self, value: &Value) -> Result<Vec<u8>> {
        Ok(match value {
            Value::Integer(v) => self.int_bytes(*v),
            Value::Buffer(bytes) => bytes.clone(),
            Value::String(text) => {
                let mut bytes = text.clone();
                bytes.push(0);
                bytes
            }
            Value::Uninitialized => Vec::new(),
            Value::Reference(r) => {
                let v = self.read_ref(r)?;
                return self.buffer_of(&v);
            }
            Value::Package(_) => return Err(RunError::Type("a buffer, not a package")),
        })
    }

    fn string_of(&mut self, value: &Value) -> Result<Vec<u8>> {
        Ok(match value {
            Value::String(text) => text.clone(),
            Value::Integer(v) => {
                let digits = if self.int32 { 8 } else { 16 };
                format!("{v:0digits$X}").into_bytes()
            }
            Value::Buffer(bytes) => {
                let parts: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
                parts.join(" ").into_bytes()
            }
            Value::Uninitialized => Vec::new(),
            Value::Reference(r) => {
                let v = self.read_ref(r)?;
                return self.string_of(&v);
            }
            Value::Package(_) => return Err(RunError::Type("a string, not a package")),
        })
    }

    fn decimal_string(&mut self, value: &Value) -> Result<Vec<u8>> {
        Ok(match value {
            Value::Buffer(bytes) => {
                let parts: Vec<String> = bytes.iter().map(|b| format!("{b}")).collect();
                parts.join(",").into_bytes()
            }
            Value::String(text) => text.clone(),
            other => format!("{}", self.to_int(other)?).into_bytes(),
        })
    }

    fn hex_string(&mut self, value: &Value) -> Result<Vec<u8>> {
        Ok(match value {
            Value::Buffer(bytes) => {
                let parts: Vec<String> = bytes.iter().map(|b| format!("0x{b:02X}")).collect();
                parts.join(",").into_bytes()
            }
            Value::String(text) => text.clone(),
            other => format!("0x{:X}", self.to_int(other)?).into_bytes(),
        })
    }

    /// Compares as the first operand's kind says.
    fn compare(&mut self, a: &Value, b: &Value) -> Result<core::cmp::Ordering> {
        let a = self.plain(a.clone())?;
        Ok(match &a {
            Value::String(x) => x.as_slice().cmp(self.string_of(b)?.as_slice()),
            Value::Buffer(x) => x.as_slice().cmp(self.buffer_of(b)?.as_slice()),
            other => self.to_int(other)?.cmp(&self.to_int(b)?),
        })
    }

    // ---- Regions and fields ---------------------------------------------------

    /// A region's space, base address and length.
    fn region(&mut self, path: &Path) -> Result<(u8, u64, u64)> {
        if let Some(region) = self.regions.get(path) {
            return Ok(*region);
        }
        let Some(Object::Region {
            space,
            offset,
            length,
        }) = self.namespace.get(path).cloned()
        else {
            return Err(RunError::NotFound(format!("{path}")));
        };
        let scope = path.parent();
        let mut base = self.eval_int(&offset, &scope)?;
        let len = self.eval_int(&length, &scope)?;
        if space == space::PCI_CONFIG {
            base = base.wrapping_add(self.pci_address(&scope));
        }
        if matches!(offset, Expr::Int(_)) && matches!(length, Expr::Int(_)) {
            self.regions.insert(path.clone(), (space, base, len));
        }
        Ok((space, base, len))
    }

    /// The PCI configuration address of the device a region is in: its
    /// `_ADR` (device and function), and the `_BBN` of the bridge above.
    fn pci_address(&mut self, scope: &Path) -> u64 {
        let mut device = scope.clone();
        let mut adr = None;
        while adr.is_none() && device.depth() > 0 {
            let candidate = device.child(*b"_ADR");
            if self.exists(&candidate) {
                adr = self.evaluate_inner(&candidate);
            } else {
                device = device.parent();
            }
        }
        let mut bus = 0;
        let mut up = device.parent();
        while up.depth() > 0 {
            let bbn = up.child(*b"_BBN");
            if self.exists(&bbn) {
                bus = self.evaluate_inner(&bbn).unwrap_or(0);
                break;
            }
            up = up.parent();
        }
        let adr = adr.unwrap_or(0);
        let (dev, function) = ((adr >> 16) & 0x1f, adr & 0x7);
        (bus & 0xff) << 20 | dev << 15 | function << 12
    }

    fn evaluate_inner(&mut self, path: &Path) -> Option<u64> {
        let value = self.call(path, Vec::new()).ok()?;
        self.to_int(&value).ok()
    }

    /// A field unit's parts: how it is reached, its bits, its access.
    fn field_parts(&self, path: &Path) -> Result<(Unit, u64, u64, u8)> {
        Ok(match self.namespace.get(path).cloned() {
            Some(Object::Field {
                region,
                bit_offset,
                bits,
                flags,
            }) => (Unit::Region(region), bit_offset.into(), bits.into(), flags),
            Some(Object::IndexField {
                index,
                data,
                bit_offset,
                bits,
                flags,
            }) => (
                Unit::Index { index, data },
                bit_offset.into(),
                bits.into(),
                flags,
            ),
            Some(Object::BankField {
                region,
                bank,
                value,
                bit_offset,
                bits,
                flags,
            }) => (
                Unit::Bank {
                    region,
                    bank,
                    value,
                },
                bit_offset.into(),
                bits.into(),
                flags,
            ),
            _ => return Err(RunError::NotFound(format!("{path}"))),
        })
    }

    fn unit_io(
        &mut self,
        unit: &Unit,
        scope: &Path,
        byte: u64,
        width: u8,
        write: Option<u64>,
    ) -> Result<u64> {
        match unit {
            Unit::Region(region) => self.region_io(region, byte, width, write),
            Unit::Bank {
                region,
                bank,
                value,
            } => {
                let selector = self.eval_int(value, scope)?;
                self.write_named(bank, Value::Integer(selector))?;
                self.region_io(region, byte, width, write)
            }
            Unit::Index { index, data } => {
                self.write_named(index, Value::Integer(byte))?;
                match write {
                    Some(v) => {
                        self.write_named(data, Value::Integer(v))?;
                        Ok(0)
                    }
                    None => {
                        let v = self.read_named(data)?;
                        self.to_int(&v)
                    }
                }
            }
        }
    }

    fn region_io(
        &mut self,
        region: &Path,
        byte: u64,
        width: u8,
        write: Option<u64>,
    ) -> Result<u64> {
        let (space, base, len) = self.region(region)?;
        if byte.saturating_add(u64::from(width)) > len {
            return Err(RunError::OutsideRegion(format!("{region}+{byte:#x}")));
        }
        let address = base.wrapping_add(byte);
        let result = match write {
            Some(value) => self.host.write(space, address, width, value).map(|()| 0),
            None => self.host.read(space, address, width),
        };
        result.map_err(|error| RunError::Host(error, space, address))
    }

    fn read_field(&mut self, path: &Path) -> Result<Value> {
        let (unit, bit, bits, flags) = self.field_parts(path)?;
        let scope = path.parent();
        let width = access_width(flags);
        let unit_bits = u64::from(width) * 8;
        let mut out = vec![0u8; bits.div_ceil(8) as usize];
        if bits > 0 {
            for index in bit / unit_bits..=(bit + bits - 1) / unit_bits {
                let value = self.unit_io(&unit, &scope, index * u64::from(width), width, None)?;
                for b in 0..unit_bits {
                    let at = index * unit_bits + b;
                    if at >= bit && at < bit + bits && (value >> b) & 1 == 1 {
                        let k = (at - bit) as usize;
                        out[k / 8] |= 1 << (k % 8);
                    }
                }
            }
        }
        Ok(self.bits_value(out, bits))
    }

    fn write_field(&mut self, path: &Path, value: &Value) -> Result<()> {
        let (unit, bit, bits, flags) = self.field_parts(path)?;
        let scope = path.parent();
        let source = self.buffer_of(value)?;
        let width = access_width(flags);
        let unit_bits = u64::from(width) * 8;
        if bits == 0 {
            return Ok(());
        }
        for index in bit / unit_bits..=(bit + bits - 1) / unit_bits {
            let mut mask = 0u64;
            let mut new = 0u64;
            for b in 0..unit_bits {
                let at = index * unit_bits + b;
                if at >= bit && at < bit + bits {
                    mask |= 1 << b;
                    let k = (at - bit) as usize;
                    if source
                        .get(k / 8)
                        .is_some_and(|byte| (byte >> (k % 8)) & 1 == 1)
                    {
                        new |= 1 << b;
                    }
                }
            }
            let full = if unit_bits == 64 {
                u64::MAX
            } else {
                (1 << unit_bits) - 1
            };
            let byte = index * u64::from(width);
            let merged = if mask == full {
                new
            } else {
                // Update rule (bits 5–6): preserve, write as ones, as zeros.
                let rest = match (flags >> 5) & 3 {
                    1 => full,
                    2 => 0,
                    _ => self.unit_io(&unit, &scope, byte, width, None)?,
                };
                (rest & !mask) | new
            };
            self.unit_io(&unit, &scope, byte, width, Some(merged))?;
        }
        Ok(())
    }

    fn bits_value(&self, bytes: Vec<u8>, bits: u64) -> Value {
        let limit = if self.int32 { 32 } else { 64 };
        if bits <= limit {
            Value::Integer(
                bytes
                    .iter()
                    .rev()
                    .fold(0u64, |v, &b| (v << 8) | u64::from(b)),
            )
        } else {
            Value::Buffer(bytes)
        }
    }

    fn read_buffer_field(&mut self, path: &Path) -> Result<Value> {
        let field = self
            .buffer_fields
            .get(path)
            .cloned()
            .ok_or_else(|| RunError::NotFound(format!("{path}")))?;
        let source = self.read_ref(&field.source)?;
        let bytes = self.buffer_of(&source)?;
        if field.bit + field.bits > bytes.len() as u64 * 8 {
            return Err(RunError::Index(bytes.len()));
        }
        let mut out = vec![0u8; field.bits.div_ceil(8) as usize];
        for k in 0..field.bits {
            let at = field.bit + k;
            if (bytes[(at / 8) as usize] >> (at % 8)) & 1 == 1 {
                out[(k / 8) as usize] |= 1 << (k % 8);
            }
        }
        Ok(self.bits_value(out, field.bits))
    }

    fn write_buffer_field(&mut self, path: &Path, value: &Value) -> Result<()> {
        let field = self
            .buffer_fields
            .get(path)
            .cloned()
            .ok_or_else(|| RunError::NotFound(format!("{path}")))?;
        let source = self.buffer_of(value)?;
        self.materialize(&field.source)?;
        let target = self.container(&field.source)?;
        let Value::Buffer(bytes) = target else {
            return Err(RunError::Type("a field of no buffer"));
        };
        if field.bit + field.bits > bytes.len() as u64 * 8 {
            return Err(RunError::Index(bytes.len()));
        }
        for k in 0..field.bits {
            let at = field.bit + k;
            let set = source
                .get((k / 8) as usize)
                .is_some_and(|b| (b >> (k % 8)) & 1 == 1);
            let byte = &mut bytes[(at / 8) as usize];
            if set {
                *byte |= 1 << (at % 8);
            } else {
                *byte &= !(1 << (at % 8));
            }
        }
        Ok(())
    }
}

/// The access width in bytes a field's flags give (bits 0–3): byte, word,
/// dword, qword; any and buffer access by byte.
fn access_width(flags: u8) -> u8 {
    match flags & 0x0f {
        2 => 2,
        3 => 4,
        4 => 8,
        _ => 1,
    }
}

fn strip_end_tag(bytes: &mut Vec<u8>) {
    if bytes.len() >= 2 && bytes[bytes.len() - 2] == 0x79 {
        bytes.truncate(bytes.len() - 2);
    }
}

fn parse_int(digits: &[u8], radix: u32) -> u64 {
    let mut v = 0u64;
    for &b in digits {
        let Some(d) = char::from(b).to_digit(radix) else {
            break;
        };
        v = v.wrapping_mul(u64::from(radix)).wrapping_add(u64::from(d));
    }
    v
}

fn from_bcd(mut v: u64) -> u64 {
    let (mut out, mut scale) = (0u64, 1u64);
    while v > 0 {
        out = out.wrapping_add((v & 0xf) * scale);
        scale = scale.wrapping_mul(10);
        v >>= 4;
    }
    out
}

fn to_bcd(mut v: u64) -> u64 {
    let (mut out, mut shift) = (0u64, 0u32);
    while v > 0 && shift < 64 {
        out |= (v % 10) << shift;
        v /= 10;
        shift += 4;
    }
    out
}

/// A path written as text (`\_SB.BAT0`, `^PCI0`, `BAT0`), as `DerefOf` of
/// a string names it.
fn parse_name(text: &str) -> Option<NameString> {
    let mut rest = text;
    let mut name = NameString::default();
    if let Some(r) = rest.strip_prefix('\\') {
        name.root = true;
        rest = r;
    }
    while let Some(r) = rest.strip_prefix('^') {
        name.up = name.up.checked_add(1)?;
        rest = r;
    }
    for part in rest.split('.').filter(|p| !p.is_empty()) {
        let bytes = part.as_bytes();
        if bytes.len() > 4 {
            return None;
        }
        let mut seg = *b"____";
        seg[..bytes.len()].copy_from_slice(bytes);
        if !crate::name::valid_seg(&seg) {
            return None;
        }
        name.segs.push(seg);
    }
    Some(name)
}
