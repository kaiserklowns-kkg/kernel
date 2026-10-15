//! ACPI Machine Language (ADR-0148): the firmware's definition blocks (the
//! DSDT and the SSDTs) read into a namespace of devices, methods, data,
//! operation regions and fields. Host-tested; no I/O.
//!
//! Loading reads every object a block defines, at the level it defines it.
//! Methods keep their bodies, to be read when they run: the arguments of a
//! call inside one can only be told once every block is loaded, since AML
//! does not mark calls and a method may be defined later.
//!
//! What runs while a block loads (`If`, `Store` and the like outside
//! methods) is read but not evaluated: the objects in both arms of an
//! `If` are defined, the first definition of a name kept. Firmware uses
//! this for objects it defines only for some operating systems.
//!
//! Firmware is untrusted: every length is checked, nesting is bounded, and
//! a block that cannot be read is an error, never a panic.

#![no_std]

extern crate alloc;

pub mod battery;
pub mod eval;
pub mod name;
pub mod parse;

use alloc::collections::BTreeMap;
use alloc::string::String;

use name::{NameString, Path, Seg};
use parse::{Arity, Error, Expr, MAX_DEPTH, Reader, op};

/// The objects the namespace holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Object {
    /// A scope with nothing else to it (the root's predefined ones).
    Scope,
    Device,
    Processor {
        id: u8,
    },
    PowerResource,
    ThermalZone,
    Method {
        args: u8,
        serialized: bool,
        /// Its body: AML read when it runs (shared: a running method
        /// keeps it while the namespace changes).
        body: alloc::rc::Rc<[u8]>,
    },
    /// A named data object (`Name`).
    Data(Expr),
    Region {
        space: u8,
        offset: Expr,
        length: Expr,
    },
    /// A field unit of a region.
    Field {
        region: Path,
        bit_offset: u32,
        bits: u32,
        flags: u8,
    },
    /// A field unit reached through an index and a data field.
    IndexField {
        index: Path,
        data: Path,
        bit_offset: u32,
        bits: u32,
        flags: u8,
    },
    /// A field unit of a region, behind a bank register.
    BankField {
        region: Path,
        bank: Path,
        value: Expr,
        bit_offset: u32,
        bits: u32,
        flags: u8,
    },
    Mutex {
        sync_level: u8,
    },
    Event,
    /// Another name for an object.
    Alias(Path),
    /// Declared to exist in another block (`External`).
    External {
        kind: u8,
        args: u8,
    },
    /// A method the interpreter answers itself (ADR-0153): `_OSI`.
    Builtin {
        args: u8,
    },
    /// A field of a buffer (`CreateDWordField` and the like, ADR-0153):
    /// where it is, the interpreter keeps.
    BufferField,
}

/// What loading one block found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Loaded {
    pub objects: usize,
    pub methods: usize,
    pub devices: usize,
    /// Definitions of a name already defined (the first is kept).
    pub duplicates: usize,
}

#[derive(Default)]
pub struct Namespace {
    objects: BTreeMap<Path, Object>,
}

/// The name `_OS` gives, as firmware expects of a current system.
pub const OS_NAME: &[u8] = b"Microsoft Windows NT";

/// The predefined scopes under the root (ACPI 6.5 §5.3.1).
const PREDEFINED: [&Seg; 5] = [b"_GPE", b"_PR_", b"_SB_", b"_SI_", b"_TZ_"];

impl Namespace {
    pub fn new() -> Self {
        let mut objects = BTreeMap::new();
        for seg in PREDEFINED {
            objects.insert(Path::root().child(*seg), Object::Scope);
        }
        // What the operating system provides (ACPI 6.5 §5.7): which
        // interfaces it has, its name, the ACPI revision, the global lock.
        let root = Path::root();
        objects.insert(root.child(*b"_OSI"), Object::Builtin { args: 1 });
        objects.insert(
            root.child(*b"_OS_"),
            Object::Data(Expr::Str(OS_NAME.to_vec())),
        );
        objects.insert(root.child(*b"_REV"), Object::Data(Expr::Int(2)));
        objects.insert(root.child(*b"_GL_"), Object::Mutex { sync_level: 0 });
        Self { objects }
    }

    pub fn get(&self, path: &Path) -> Option<&Object> {
        self.objects.get(path)
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Path, &Object)> {
        self.objects.iter()
    }

    /// Puts `object` at `path` (a method's own objects while it runs,
    /// ADR-0153), replacing what was there.
    pub(crate) fn insert(&mut self, path: Path, object: Object) {
        self.objects.insert(path, object);
    }

    pub(crate) fn remove(&mut self, path: &Path) {
        self.objects.remove(path);
    }

    /// The object `name` refers to from `scope`, following the search rules
    /// (a lone segment is looked for in the scope and each one above it)
    /// and aliases. Returns its path.
    pub fn lookup(&self, name: &NameString, scope: &Path) -> Option<Path> {
        let mut found = if name.searches_up() {
            let mut scope = scope.clone();
            loop {
                let candidate = scope.child(name.segs[0]);
                if self.objects.contains_key(&candidate) {
                    break Some(candidate);
                }
                if scope.depth() == 0 {
                    break None;
                }
                scope = scope.parent();
            }
        } else {
            name.resolve(scope).filter(|p| self.objects.contains_key(p))
        }?;
        // Aliases, not around in a circle.
        for _ in 0..8 {
            match self.objects.get(&found) {
                Some(Object::Alias(target)) => found = target.clone(),
                _ => return Some(found),
            }
        }
        None
    }

    /// The devices, each with its hardware ID (`_HID`) if it is a constant:
    /// an EISA ID as text, or a string.
    pub fn devices(&self) -> impl Iterator<Item = (&Path, Option<String>)> {
        self.objects
            .iter()
            .filter(|(_, object)| matches!(object, Object::Device))
            .map(|(path, _)| (path, self.hardware_id(path)))
    }

    /// A device's `_HID`, if it is a constant.
    pub fn hardware_id(&self, device: &Path) -> Option<String> {
        match self.objects.get(&device.child(*b"_HID"))? {
            Object::Data(Expr::Int(value)) => {
                let id = name::eisa_id(*value as u32);
                Some(String::from_utf8_lossy(&id).into_owned())
            }
            Object::Data(Expr::Str(text)) => Some(String::from_utf8_lossy(text).into_owned()),
            _ => None,
        }
    }

    /// Reads one definition block's AML (the table without its 36-byte
    /// header) into the namespace.
    pub fn load(&mut self, aml: &[u8]) -> Result<Loaded, Error> {
        let mut loader = Loader {
            namespace: self,
            loaded: Loaded::default(),
        };
        let mut reader = Reader::new(aml);
        loader.terms(&mut reader, &Path::root(), aml.len(), 0)?;
        Ok(loader.loaded)
    }

    fn define(&mut self, path: Path, object: Object, loaded: &mut Loaded) {
        let external = matches!(object, Object::External { .. });
        // An `External` gives way to the definition, never the reverse.
        let replace = match self.objects.get(&path) {
            None => true,
            Some(Object::External { .. }) => !external,
            Some(Object::Scope) => !external,
            Some(_) if external => false,
            Some(_) => {
                loaded.duplicates += 1;
                false
            }
        };
        if replace {
            match object {
                Object::Method { .. } => loaded.methods += 1,
                Object::Device => loaded.devices += 1,
                _ => {}
            }
            loaded.objects += 1;
            self.objects.insert(path, object);
        }
    }
}

/// Method arities from the namespace being loaded.
pub(crate) struct Methods<'a> {
    pub(crate) namespace: &'a Namespace,
    pub(crate) scope: &'a Path,
}

impl Arity for Methods<'_> {
    fn arity(&self, name: &NameString) -> Option<u8> {
        let path = self.namespace.lookup(name, self.scope)?;
        match self.namespace.objects.get(&path)? {
            Object::Method { args, .. } => Some(*args),
            Object::External { kind: 8, args } => Some(*args),
            Object::Builtin { args } => Some(*args),
            _ => None,
        }
    }
}

pub(crate) struct Loader<'a> {
    pub(crate) namespace: &'a mut Namespace,
    pub(crate) loaded: Loaded,
}

impl Loader<'_> {
    /// The terms up to `end`, in `scope`.
    fn terms(
        &mut self,
        r: &mut Reader<'_>,
        scope: &Path,
        end: usize,
        depth: usize,
    ) -> Result<(), Error> {
        if depth > MAX_DEPTH {
            return Err(Error::TooDeep(r.at));
        }
        while r.at < end {
            self.term(r, scope, end, depth)?;
        }
        if r.at != end {
            return Err(Error::Malformed(r.at));
        }
        Ok(())
    }

    /// The path `name` defines in `scope`.
    fn defined(&self, r: &Reader<'_>, name: &NameString, scope: &Path) -> Result<Path, Error> {
        match name.resolve(scope) {
            Some(path) if !name.segs.is_empty() => Ok(path),
            _ => Err(Error::Malformed(r.at)),
        }
    }

    pub(crate) fn term(
        &mut self,
        r: &mut Reader<'_>,
        scope: &Path,
        end: usize,
        depth: usize,
    ) -> Result<(), Error> {
        let at = r.at;
        if r.at_name() {
            // A call outside a method, or a name alone (nothing to define).
            let methods = Methods {
                namespace: self.namespace,
                scope,
            };
            r.term(&methods, depth)?;
            return Ok(());
        }
        let opcode = r.opcode()?;
        match opcode {
            op::SCOPE | op::DEVICE | op::POWER_RES | op::THERMAL_ZONE | op::PROCESSOR => {
                let block_end = r.pkg_length(end)?;
                let name = r.name()?;
                // `Scope (\)` opens the root itself.
                let path = if opcode == op::SCOPE {
                    name.resolve(scope).ok_or(Error::Malformed(r.at))?
                } else {
                    self.defined(r, &name, scope)?
                };
                let object = match opcode {
                    op::SCOPE => None,
                    op::DEVICE => Some(Object::Device),
                    op::THERMAL_ZONE => Some(Object::ThermalZone),
                    op::POWER_RES => {
                        r.take(3)?; // system level, resource order
                        Some(Object::PowerResource)
                    }
                    _ => {
                        let id = r.byte()?;
                        r.take(5)?; // PBLK address and length
                        Some(Object::Processor { id })
                    }
                };
                if let Some(object) = object {
                    self.namespace
                        .define(path.clone(), object, &mut self.loaded);
                } else if !self.namespace.objects.contains_key(&path) && path.depth() > 0 {
                    // A scope opened on what another block will define:
                    // its contents still have somewhere to be.
                    self.namespace.objects.insert(path.clone(), Object::Scope);
                }
                self.terms(r, &path, block_end, depth + 1)
            }
            op::METHOD => {
                let block_end = r.pkg_length(end)?;
                let name = r.name()?;
                let path = self.defined(r, &name, scope)?;
                let flags = r.byte()?;
                if r.at > block_end {
                    return Err(Error::Malformed(at));
                }
                let body: alloc::rc::Rc<[u8]> = r.bytes[r.at..block_end].into();
                r.at = block_end;
                let method = Object::Method {
                    args: flags & 7,
                    serialized: flags & 8 != 0,
                    body,
                };
                self.namespace.define(path, method, &mut self.loaded);
                Ok(())
            }
            op::NAME => {
                let name = r.name()?;
                let path = self.defined(r, &name, scope)?;
                let methods = Methods {
                    namespace: self.namespace,
                    scope,
                };
                let value = r.term(&methods, depth + 1)?;
                self.namespace
                    .define(path, Object::Data(value), &mut self.loaded);
                Ok(())
            }
            op::ALIAS => {
                let source = r.name()?;
                let alias = r.name()?;
                let target = source.resolve(scope).ok_or(Error::Malformed(at))?;
                let path = self.defined(r, &alias, scope)?;
                self.namespace
                    .define(path, Object::Alias(target), &mut self.loaded);
                Ok(())
            }
            op::EXTERNAL => {
                let name = r.name()?;
                let kind = r.byte()?;
                let args = r.byte()?;
                let path = self.defined(r, &name, scope)?;
                self.namespace
                    .define(path, Object::External { kind, args }, &mut self.loaded);
                Ok(())
            }
            op::MUTEX => {
                let name = r.name()?;
                let sync_level = r.byte()? & 0x0f;
                let path = self.defined(r, &name, scope)?;
                self.namespace
                    .define(path, Object::Mutex { sync_level }, &mut self.loaded);
                Ok(())
            }
            op::EVENT => {
                let name = r.name()?;
                let path = self.defined(r, &name, scope)?;
                self.namespace.define(path, Object::Event, &mut self.loaded);
                Ok(())
            }
            op::REGION => {
                let name = r.name()?;
                let space = r.byte()?;
                let methods = Methods {
                    namespace: self.namespace,
                    scope,
                };
                let offset = r.term(&methods, depth + 1)?;
                let length = r.term(&methods, depth + 1)?;
                let path = self.defined(r, &name, scope)?;
                let region = Object::Region {
                    space,
                    offset,
                    length,
                };
                self.namespace.define(path, region, &mut self.loaded);
                Ok(())
            }
            op::DATA_REGION => {
                let name = r.name()?;
                let methods = Methods {
                    namespace: self.namespace,
                    scope,
                };
                let signature = r.term(&methods, depth + 1)?;
                let oem = r.term(&methods, depth + 1)?;
                let table = r.term(&methods, depth + 1)?;
                let path = self.defined(r, &name, scope)?;
                // Kept as the expression that finds its table.
                let region = Object::Data(Expr::Op(
                    op::DATA_REGION,
                    alloc::vec![signature, oem, table],
                ));
                self.namespace.define(path, region, &mut self.loaded);
                Ok(())
            }
            op::FIELD | op::INDEX_FIELD | op::BANK_FIELD => {
                self.field(r, scope, end, opcode, depth)
            }
            op::IF | op::WHILE => {
                let block_end = r.pkg_length(end)?;
                let methods = Methods {
                    namespace: self.namespace,
                    scope,
                };
                r.term(&methods, depth + 1)?;
                self.terms(r, scope, block_end, depth + 1)
            }
            op::ELSE => {
                let block_end = r.pkg_length(end)?;
                self.terms(r, scope, block_end, depth + 1)
            }
            _ => {
                // Something run at load time (`Store`, `CreateDWordField`,
                // …): read past it. Buffer fields it creates are defined
                // when it runs, not here.
                r.at = at;
                let methods = Methods {
                    namespace: self.namespace,
                    scope,
                };
                r.term(&methods, depth)?;
                Ok(())
            }
        }
    }

    /// A `Field`, `IndexField` or `BankField` and its list of units
    /// (§20.2.5.2): named units, reserved gaps, access changes.
    fn field(
        &mut self,
        r: &mut Reader<'_>,
        scope: &Path,
        end: usize,
        opcode: u16,
        depth: usize,
    ) -> Result<(), Error> {
        let at = r.at;
        let block_end = r.pkg_length(end)?;
        let first = r.name()?;
        let first = first.resolve(scope).ok_or(Error::Malformed(at))?;
        let (second, value) = match opcode {
            op::INDEX_FIELD => (
                Some(r.name()?.resolve(scope).ok_or(Error::Malformed(at))?),
                None,
            ),
            op::BANK_FIELD => {
                let bank = r.name()?.resolve(scope).ok_or(Error::Malformed(at))?;
                let methods = Methods {
                    namespace: self.namespace,
                    scope,
                };
                (Some(bank), Some(r.term(&methods, depth + 1)?))
            }
            _ => (None, None),
        };
        let mut flags = r.byte()?;
        let mut bit_offset: u32 = 0;
        while r.at < block_end {
            match r.peek()? {
                // ReservedField: a gap.
                0x00 => {
                    r.at += 1;
                    bit_offset = bit_offset.saturating_add(field_length(r)?);
                }
                // AccessField: access type and attribute from here on.
                0x01 => {
                    r.at += 1;
                    let access = r.byte()?;
                    r.byte()?;
                    flags = (flags & !0x0f) | (access & 0x0f);
                }
                // ConnectField: a resource or a name; nothing to define.
                0x02 => {
                    r.at += 1;
                    if r.at_name() {
                        r.name()?;
                    } else {
                        let methods = Methods {
                            namespace: self.namespace,
                            scope,
                        };
                        r.term(&methods, depth + 1)?;
                    }
                }
                // ExtendedAccessField.
                0x03 => {
                    r.at += 1;
                    let access = r.byte()?;
                    r.take(2)?;
                    flags = (flags & !0x0f) | (access & 0x0f);
                }
                _ => {
                    let at = r.at;
                    let seg: Seg = r.take(4)?.try_into().expect("four bytes");
                    if !name::valid_seg(&seg) {
                        return Err(Error::Malformed(at));
                    }
                    let bits = field_length(r)?;
                    let unit = match (opcode, &second) {
                        (op::INDEX_FIELD, Some(data)) => Object::IndexField {
                            index: first.clone(),
                            data: data.clone(),
                            bit_offset,
                            bits,
                            flags,
                        },
                        (op::BANK_FIELD, Some(bank)) => Object::BankField {
                            region: first.clone(),
                            bank: bank.clone(),
                            value: value.clone().unwrap_or(Expr::Int(0)),
                            bit_offset,
                            bits,
                            flags,
                        },
                        _ => Object::Field {
                            region: first.clone(),
                            bit_offset,
                            bits,
                            flags,
                        },
                    };
                    self.namespace
                        .define(scope.child(seg), unit, &mut self.loaded);
                    bit_offset = bit_offset.saturating_add(bits);
                }
            }
        }
        if r.at != block_end {
            return Err(Error::Malformed(at));
        }
        Ok(())
    }
}

/// A field unit's length in bits: a package length's encoding, without the
/// meaning of an end.
fn field_length(r: &mut Reader<'_>) -> Result<u32, Error> {
    let lead = r.byte()?;
    let follow = usize::from(lead >> 6);
    if follow == 0 {
        return Ok(u32::from(lead & 0x3f));
    }
    let mut length = u32::from(lead & 0x0f);
    for i in 0..follow {
        length |= u32::from(r.byte()?) << (4 + 8 * i);
    }
    Ok(length)
}

#[cfg(test)]
mod tests;
