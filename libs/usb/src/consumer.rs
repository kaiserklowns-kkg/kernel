//! HID consumer controls (HID Usage Tables §15; ADR-0102): the media keys
//! of USB keyboards, sent apart from the boot keyboard's keys, on an
//! interface of their own or in a report of their own.
//!
//! - [`Layout::parse`] reads a report descriptor and finds the first
//!   application collection of usage Consumer Control that has a key this
//!   module knows ([`Key`]), as a bit of its own (a variable field) or as
//!   values of an array.
//! - [`Keys`] turns its input reports into the bytes keyboards send for
//!   those keys (`oceans_abi::display::KEY_*`), one for each key newly
//!   pressed.
//!
//! As for pointers, descriptors and reports come from the device and are
//! parsed defensively.

use crate::pointer::{
    APPLICATION, COLLECTION, CONSTANT, END_COLLECTION, Field, GLOBAL, Globals, INPUT, LOCAL,
    LONG_ITEM, Locals, MAIN, MAX_DEPTH, MAX_FIELD_BITS, MAX_PUSH, MAX_REPORT, MAX_USAGES,
    VARIABLE, sign_extend,
};

/// The Consumer page, and its Consumer Control collection.
const CONSUMER: u32 = 0x0c;
const CONSUMER_CONTROL: u32 = CONSUMER << 16 | 0x01;

/// Variable fields and arrays kept, at most.
const MAX_CONTROLS: usize = 16;
const MAX_ARRAYS: usize = 4;

/// The keys known: what they send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Mute,
    VolumeDown,
    VolumeUp,
    PlayPause,
    Stop,
    Previous,
    Next,
}

impl Key {
    pub const ALL: [Key; 7] = [
        Key::Mute,
        Key::VolumeDown,
        Key::VolumeUp,
        Key::PlayPause,
        Key::Stop,
        Key::Previous,
        Key::Next,
    ];

    /// The key of a consumer usage (Mute 0xe2, Volume Increment 0xe9 and
    /// Decrement 0xea, Play/Pause 0xcd, Stop 0xb7, Scan Previous Track
    /// 0xb6 and Next Track 0xb5).
    pub fn of(usage: u32) -> Option<Self> {
        if usage >> 16 != CONSUMER {
            return None;
        }
        Some(match usage & 0xffff {
            0xe2 => Key::Mute,
            0xea => Key::VolumeDown,
            0xe9 => Key::VolumeUp,
            0xcd => Key::PlayPause,
            0xb7 => Key::Stop,
            0xb6 => Key::Previous,
            0xb5 => Key::Next,
            _ => return None,
        })
    }

    /// The byte keyboards send for it (`oceans_abi::display`: `KEY_MUTE`,
    /// `KEY_VOLUME_DOWN`, `KEY_VOLUME_UP`, `KEY_PLAY_PAUSE`, `KEY_STOP`,
    /// `KEY_PREVIOUS`, `KEY_NEXT`).
    pub fn byte(self) -> u8 {
        match self {
            Key::Mute => 0x8c,
            Key::VolumeDown => 0x8d,
            Key::VolumeUp => 0x8e,
            Key::PlayPause => 0xa0,
            Key::Stop => 0xa1,
            Key::Previous => 0xa2,
            Key::Next => 0xa3,
        }
    }

    fn bit(self) -> u8 {
        1 << Key::ALL.iter().position(|&k| k == self).unwrap_or(0)
    }
}

/// Why a report descriptor was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// As for pointers: items past the end, collections that do not nest,
    /// fields too wide, reports too long.
    Malformed,
    /// No Consumer Control collection with a key this module knows.
    NoKeys,
}

/// An array field: `count` values of `field.bits` bits from `field.offset`,
/// each a usage of the list (or range) it was declared with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Array {
    field: Field,
    count: u32,
    stride: u32,
    usages: Locals,
}

impl Array {
    /// The usage a value names: the `value - min`th of the list or range.
    fn usage(&self, value: i32) -> Option<u32> {
        let index = u32::try_from(i64::from(value) - i64::from(self.field.min)).ok()?;
        if value < self.field.min || value > self.field.max {
            return None;
        }
        if self.usages.count > 0 {
            return self.usages.usages.get(index as usize).copied().filter(|_| {
                (index as usize) < self.usages.count
            });
        }
        let (min, max) = self.usages.range?;
        let usage = min.checked_add(index)?;
        (usage <= max).then_some(usage)
    }
}

/// The consumer controls' input report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The report ID that carries them, when the device numbers reports.
    pub report_id: Option<u8>,
    controls: [Option<(Field, Key)>; MAX_CONTROLS],
    arrays: [Option<Array>; MAX_ARRAYS],
}

impl Layout {
    /// Finds the consumer controls in a report descriptor.
    pub fn parse(descriptor: &[u8]) -> Result<Self, Error> {
        let mut globals = Globals::default();
        let mut stack = [Globals::default(); MAX_PUSH];
        let mut pushed = 0;
        let mut locals = Locals::default();
        let mut offsets = [0u32; 256];
        let mut uses_ids = false;
        let mut depth = 0usize;
        let mut inside: Option<usize> = None;
        let mut found = Found::default();
        let mut at = 0;
        while at < descriptor.len() {
            let prefix = descriptor[at];
            if prefix == LONG_ITEM {
                let size = usize::from(*descriptor.get(at + 1).ok_or(Error::Malformed)?);
                at += 3 + size;
                if at > descriptor.len() {
                    return Err(Error::Malformed);
                }
                continue;
            }
            let size = [0, 1, 2, 4][usize::from(prefix & 3)];
            let data = descriptor
                .get(at + 1..at + 1 + size)
                .ok_or(Error::Malformed)?;
            at += 1 + size;
            let mut value = 0u32;
            for (i, byte) in data.iter().enumerate() {
                value |= u32::from(*byte) << (8 * i);
            }
            match (prefix >> 2 & 3, prefix >> 4) {
                (GLOBAL, 0x0) => globals.page = value,
                (GLOBAL, 0x1) => globals.min = sign_extend(value, size as u8),
                (GLOBAL, 0x2) => {
                    globals.max_raw = value;
                    globals.max_size = size as u8;
                }
                (GLOBAL, 0x7) => globals.size = value,
                (GLOBAL, 0x8) => {
                    if value == 0 || value > 0xff {
                        return Err(Error::Malformed);
                    }
                    globals.report_id = value as u8;
                    uses_ids = true;
                }
                (GLOBAL, 0x9) => globals.count = value,
                (GLOBAL, 0xa) => {
                    *stack.get_mut(pushed).ok_or(Error::Malformed)? = globals;
                    pushed += 1;
                }
                (GLOBAL, 0xb) => {
                    pushed = pushed.checked_sub(1).ok_or(Error::Malformed)?;
                    globals = stack[pushed];
                }
                (GLOBAL, _) => {}
                (LOCAL, 0x0) => {
                    if locals.count < MAX_USAGES {
                        locals.usages[locals.count] = Locals::full(globals.page, value, size);
                        locals.count += 1;
                    }
                }
                (LOCAL, 0x1) => locals.minimum = Some(Locals::full(globals.page, value, size)),
                (LOCAL, 0x2) => {
                    let max = Locals::full(globals.page, value, size);
                    let min = locals.minimum.unwrap_or(max);
                    if min <= max {
                        locals.range = Some((min, max));
                    }
                }
                (LOCAL, _) => {}
                (MAIN, COLLECTION) => {
                    depth += 1;
                    if depth > MAX_DEPTH {
                        return Err(Error::Malformed);
                    }
                    if inside.is_none()
                        && found.is_empty()
                        && value == APPLICATION
                        && locals.usage(0) == Some(CONSUMER_CONTROL)
                    {
                        inside = Some(depth);
                    }
                    locals = Locals::default();
                }
                (MAIN, END_COLLECTION) => {
                    if inside == Some(depth) {
                        inside = None;
                        if !found.is_empty() {
                            return Ok(found.layout(uses_ids));
                        }
                    }
                    depth = depth.checked_sub(1).ok_or(Error::Malformed)?;
                    locals = Locals::default();
                }
                (MAIN, INPUT) => {
                    let id = usize::from(globals.report_id);
                    let bits = globals.size;
                    if bits > MAX_FIELD_BITS || globals.count > 8 * MAX_REPORT as u32 {
                        return Err(Error::Malformed);
                    }
                    let start = offsets[id];
                    offsets[id] = start + bits * globals.count;
                    if offsets[id] > 8 * MAX_REPORT as u32 {
                        return Err(Error::Malformed);
                    }
                    let same = found.report_id.is_none_or(|known| known == globals.report_id);
                    if inside.is_some() && value & CONSTANT == 0 && bits > 0 && same {
                        found.collect(&globals, &locals, start, value & VARIABLE != 0);
                    }
                    locals = Locals::default();
                }
                (MAIN, _) => locals = Locals::default(),
                _ => return Err(Error::Malformed),
            }
        }
        if depth != 0 {
            return Err(Error::Malformed);
        }
        Err(Error::NoKeys)
    }

    /// The keys held in one input report, as bits (`Key::ALL`'s order);
    /// `None` for another report ID.
    fn held(&self, report: &[u8]) -> Option<u8> {
        let data = match self.report_id {
            Some(id) => {
                let (&first, rest) = report.split_first()?;
                if first != id {
                    return None;
                }
                rest
            }
            None => report,
        };
        let mut held = 0;
        for (field, key) in self.controls.iter().flatten() {
            if field.read(data).is_some_and(|value| value != 0) {
                held |= key.bit();
            }
        }
        for array in self.arrays.iter().flatten() {
            for slot in 0..array.count {
                let field = Field {
                    offset: array.field.offset + slot * array.stride,
                    ..array.field
                };
                let key = field
                    .read(data)
                    .and_then(|value| array.usage(value))
                    .and_then(Key::of);
                if let Some(key) = key {
                    held |= key.bit();
                }
            }
        }
        Some(held)
    }
}

/// What was found inside the collection so far.
#[derive(Default)]
struct Found {
    report_id: Option<u8>,
    controls: [Option<(Field, Key)>; MAX_CONTROLS],
    arrays: [Option<Array>; MAX_ARRAYS],
}

impl Found {
    fn is_empty(&self) -> bool {
        self.controls.iter().all(Option::is_none) && self.arrays.iter().all(Option::is_none)
    }

    fn layout(&self, uses_ids: bool) -> Layout {
        Layout {
            report_id: uses_ids.then_some(self.report_id.unwrap_or(0)),
            controls: self.controls,
            arrays: self.arrays,
        }
    }

    /// Records the known keys of one input item.
    fn collect(&mut self, globals: &Globals, locals: &Locals, start: u32, variable: bool) {
        let field = Field {
            offset: start,
            bits: globals.size as u8,
            min: globals.min,
            max: globals.max(),
        };
        if variable {
            for index in 0..globals.count {
                let Some(key) = locals.usage(index).and_then(Key::of) else {
                    continue;
                };
                if let Some(slot) = self.controls.iter_mut().find(|s| s.is_none()) {
                    *slot = Some((
                        Field {
                            offset: start + index * globals.size,
                            ..field
                        },
                        key,
                    ));
                    self.report_id.get_or_insert(globals.report_id);
                }
            }
            return;
        }
        let array = Array {
            field,
            count: globals.count,
            stride: globals.size,
            usages: *locals,
        };
        // Kept only if one of its values names a key known.
        let knows = (array.field.min..=array.field.max)
            .take(0x400)
            .any(|value| array.usage(value).and_then(Key::of).is_some());
        if knows && let Some(slot) = self.arrays.iter_mut().find(|s| s.is_none()) {
            *slot = Some(array);
            self.report_id.get_or_insert(globals.report_id);
        }
    }
}

/// A consumer-control device's keys between reports: each key newly
/// pressed becomes its byte.
pub struct Keys {
    layout: Layout,
    held: u8,
}

impl Keys {
    pub const fn new(layout: Layout) -> Self {
        Self { layout, held: 0 }
    }

    /// Feeds one input report; calls `emit` with the byte of each key newly
    /// pressed (in [`Key::ALL`]'s order).
    pub fn report(&mut self, report: &[u8], mut emit: impl FnMut(u8)) {
        let Some(held) = self.layout.held(report) else {
            return;
        };
        for key in Key::ALL {
            if held & key.bit() != 0 && self.held & key.bit() == 0 {
                emit(key.byte());
            }
        }
        self.held = held;
    }
}

#[cfg(test)]
mod tests;
