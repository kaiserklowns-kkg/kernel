//! HID pointers (HID 1.11; ADR-0042): mice and tablets.
//!
//! - [`Layout::parse`] reads a report descriptor (§6.2.2) and finds the
//!   pointer in it: the first application collection of usage Generic
//!   Desktop Mouse or Pointer that has X and Y inputs. It records where
//!   the buttons, X, Y, the wheel and the horizontal wheel (Consumer AC
//!   Pan) are in that collection's input report, and whether X and Y are
//!   relative (a mouse) or absolute (a tablet).
//! - [`Layout::decode`] turns one input report into an
//!   [`oceans_input::Sample`].
//! - [`boot_report`] decodes the fixed boot protocol report of a mouse
//!   (appendix B.2): buttons, X, Y and, where the device sends a fourth
//!   byte, the wheel.
//!
//! Report descriptors and reports come from the device: they are parsed
//! defensively. Malformed descriptors are refused rather than guessed at,
//! and short reports decode to nothing.

use oceans_input::{Axes, MAX_BUTTONS, Sample};

/// A boot protocol mouse report: buttons, X, Y; the wheel is a fourth
/// byte where present.
pub const BOOT_REPORT_MIN: usize = 3;
/// The longest report this module decodes (a full-speed interrupt
/// packet).
pub const MAX_REPORT: usize = 64;

/// Usage pages and usages (HID Usage Tables 1.4), as `page << 16 | id`.
mod usage {
    pub const GENERIC_DESKTOP: u32 = 0x01;
    pub const BUTTON: u32 = 0x09;
    pub const CONSUMER: u32 = 0x0c;
    pub const POINTER: u32 = GENERIC_DESKTOP << 16 | 0x01;
    pub const MOUSE: u32 = GENERIC_DESKTOP << 16 | 0x02;
    pub const X: u32 = GENERIC_DESKTOP << 16 | 0x30;
    pub const Y: u32 = GENERIC_DESKTOP << 16 | 0x31;
    pub const WHEEL: u32 = GENERIC_DESKTOP << 16 | 0x38;
    pub const AC_PAN: u32 = CONSUMER << 16 | 0x0238;
}

/// Item types and main item tags (§6.2.2.4–6).
const MAIN: u8 = 0;
const GLOBAL: u8 = 1;
const LOCAL: u8 = 2;
const INPUT: u8 = 0x8;
const COLLECTION: u8 = 0xa;
const END_COLLECTION: u8 = 0xc;
/// Input item flags.
const CONSTANT: u32 = 1 << 0;
const VARIABLE: u32 = 1 << 1;
const RELATIVE: u32 = 1 << 2;
const APPLICATION: u32 = 1;
const LONG_ITEM: u8 = 0xfe;

/// Global state stack depth (`PUSH`); collection nesting.
const MAX_PUSH: usize = 4;
const MAX_DEPTH: usize = 16;
/// Usages a local state remembers (more are ignored).
const MAX_USAGES: usize = 16;
/// The largest field: 32 bits.
const MAX_FIELD_BITS: u32 = 32;

/// Why a report descriptor was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Items run past the end, collections do not nest, a field is wider
    /// than 32 bits, or a report is larger than [`MAX_REPORT`].
    Malformed,
    /// Well-formed, but without a mouse or pointer collection with X and Y.
    NotAPointer,
}

/// Where one value sits in a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// Bit offset from the start of the report data (after the report ID).
    pub offset: u32,
    /// Width in bits, 1 to 32.
    pub bits: u8,
    pub min: i32,
    pub max: i32,
}

impl Field {
    /// The raw value: `bits` bits from `offset`, least significant first,
    /// sign-extended when the field's range is signed. `None` past the end
    /// of `data`.
    fn read(&self, data: &[u8]) -> Option<i32> {
        let bits = u32::from(self.bits);
        let end = self.offset.checked_add(bits)?;
        if end.div_ceil(8) as usize > data.len() || bits == 0 || bits > MAX_FIELD_BITS {
            return None;
        }
        let mut value = 0u64;
        for bit in 0..bits {
            let at = self.offset + bit;
            let set = data[(at / 8) as usize] >> (at % 8) & 1;
            value |= u64::from(set) << bit;
        }
        if self.min < 0 && value & 1 << (bits - 1) != 0 {
            value |= !0u64 << bits;
        }
        Some(value as i64 as i32)
    }
}

/// A pointer's input report, as its report descriptor describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The report ID that carries the pointer, when the device numbers its
    /// reports (the report's first byte).
    pub report_id: Option<u8>,
    /// Buttons by number (index 0 is button 1).
    pub buttons: [Option<Field>; MAX_BUTTONS as usize],
    pub x: Field,
    pub y: Field,
    pub wheel: Option<Field>,
    pub pan: Option<Field>,
    /// X and Y are positions (a tablet), not motion (a mouse).
    pub absolute: bool,
}

/// The parser's global state (§6.2.2.7).
#[derive(Clone, Copy, Default)]
struct Globals {
    page: u32,
    min: i32,
    max_raw: u32,
    max_size: u8,
    size: u32,
    count: u32,
    report_id: u8,
}

impl Globals {
    /// Logical maximum: signed when the minimum is negative, otherwise
    /// unsigned (devices write 255 as one byte `0xff`).
    fn max(&self) -> i32 {
        if self.min < 0 {
            sign_extend(self.max_raw, self.max_size)
        } else {
            self.max_raw.min(i32::MAX as u32) as i32
        }
    }
}

/// The parser's local state (§6.2.2.8): cleared after every main item.
#[derive(Clone, Copy, Default)]
struct Locals {
    usages: [u32; MAX_USAGES],
    count: usize,
    range: Option<(u32, u32)>,
    minimum: Option<u32>,
}

impl Locals {
    /// The usage of field `index` of a main item: listed usages in order
    /// (the last repeats), else a usage range.
    fn usage(&self, index: u32) -> Option<u32> {
        if self.count > 0 {
            return Some(self.usages[(index as usize).min(self.count - 1)]);
        }
        let (min, max) = self.range?;
        Some(min.saturating_add(index).min(max))
    }

    /// With a 1–2 byte usage, the page comes from the global state.
    fn full(page: u32, value: u32, size: usize) -> u32 {
        if size == 4 { value } else { page << 16 | value }
    }
}

fn sign_extend(value: u32, size: u8) -> i32 {
    match size {
        1 => value as u8 as i8 as i32,
        2 => value as u16 as i16 as i32,
        _ => value as i32,
    }
}

/// Fields found inside the pointer collection so far.
#[derive(Default)]
struct Found {
    report_id: Option<u8>,
    buttons: [Option<Field>; MAX_BUTTONS as usize],
    x: Option<(Field, bool)>,
    y: Option<(Field, bool)>,
    wheel: Option<Field>,
    pan: Option<Field>,
}

impl Layout {
    /// Finds the pointer in a report descriptor.
    pub fn parse(descriptor: &[u8]) -> Result<Self, Error> {
        let mut globals = Globals::default();
        let mut stack = [Globals::default(); MAX_PUSH];
        let mut pushed = 0;
        let mut locals = Locals::default();
        // Bits of input report so far, per report ID.
        let mut offsets = [0u32; 256];
        let mut uses_ids = false;
        let mut depth = 0usize;
        // The depth of the pointer application collection we are in.
        let mut pointer: Option<usize> = None;
        let mut found = Found::default();
        let mut complete = false;
        let mut at = 0;
        while at < descriptor.len() {
            let prefix = descriptor[at];
            if prefix == LONG_ITEM {
                // Long items (§6.2.2.3) carry nothing a pointer needs.
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
            let tag = prefix >> 4;
            match (prefix >> 2 & 3, tag) {
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
                    let usage = locals.usage(0);
                    if pointer.is_none()
                        && found.x.is_none()
                        && value == APPLICATION
                        && matches!(usage, Some(usage::MOUSE | usage::POINTER))
                    {
                        pointer = Some(depth);
                    }
                    locals = Locals::default();
                }
                (MAIN, END_COLLECTION) => {
                    if pointer == Some(depth) {
                        pointer = None;
                        if found.x.is_some() && found.y.is_some() {
                            complete = true;
                            break;
                        }
                        // An application without X and Y: keep looking.
                        found = Found::default();
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
                    let total = bits * globals.count;
                    offsets[id] = start + total;
                    if offsets[id] > 8 * MAX_REPORT as u32 {
                        return Err(Error::Malformed);
                    }
                    if pointer.is_some() && value & (CONSTANT | VARIABLE) == VARIABLE && bits > 0 {
                        let same_report = found
                            .report_id
                            .is_none_or(|known| known == globals.report_id);
                        if same_report {
                            collect(&mut found, &globals, &locals, start, value);
                        }
                    }
                    locals = Locals::default();
                }
                (MAIN, _) => locals = Locals::default(),
                _ => return Err(Error::Malformed),
            }
        }
        // A descriptor cut short leaves collections open.
        if !complete && depth != 0 {
            return Err(Error::Malformed);
        }
        let (Some((x, x_relative)), Some((y, y_relative))) = (found.x, found.y) else {
            return Err(Error::NotAPointer);
        };
        if x_relative != y_relative || (!x_relative && (x.max <= x.min || y.max <= y.min)) {
            return Err(Error::NotAPointer);
        }
        Ok(Self {
            report_id: uses_ids.then_some(found.report_id.unwrap_or(0)),
            buttons: found.buttons,
            x,
            y,
            wheel: found.wheel,
            pan: found.pan,
            absolute: !x_relative,
        })
    }

    /// The buttons the pointer has.
    pub fn button_count(&self) -> usize {
        self.buttons.iter().flatten().count()
    }

    pub fn axes(&self) -> Axes {
        if self.absolute {
            Axes::Absolute {
                x_max: span(&self.x) as u32,
                y_max: span(&self.y) as u32,
            }
        } else {
            Axes::Relative
        }
    }

    /// Decodes one input report: `None` for another report ID or one too
    /// short to hold X and Y. Absolute positions are clamped into the
    /// logical range and moved to start at 0.
    pub fn decode(&self, report: &[u8]) -> Option<Sample> {
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
        let mut x = self.x.read(data)?;
        let mut y = self.y.read(data)?;
        if self.absolute {
            x = from_origin(&self.x, x);
            y = from_origin(&self.y, y);
        }
        let mut buttons = 0u32;
        for (bit, field) in self.buttons.iter().enumerate() {
            if let Some(field) = field
                && field.read(data).is_some_and(|value| value != 0)
            {
                buttons |= 1 << bit;
            }
        }
        let delta = |field: Option<Field>| field.and_then(|f| f.read(data)).unwrap_or(0);
        Some(Sample {
            buttons,
            x,
            y,
            wheel: delta(self.wheel),
            pan: delta(self.pan),
        })
    }
}

/// An absolute field's range, `max - min`, capped to what a sample holds.
fn span(field: &Field) -> i32 {
    (i64::from(field.max) - i64::from(field.min)).clamp(0, i64::from(i32::MAX)) as i32
}

/// An absolute value clamped into its field's range, counted from `min`.
fn from_origin(field: &Field, value: i32) -> i32 {
    let value = i64::from(value.clamp(field.min, field.max)) - i64::from(field.min);
    value.min(i64::from(span(field))) as i32
}

/// Records the fields of one variable input item of the pointer.
fn collect(found: &mut Found, globals: &Globals, locals: &Locals, start: u32, flags: u32) {
    let relative = flags & RELATIVE != 0;
    for index in 0..globals.count {
        let Some(usage) = locals.usage(index) else {
            return;
        };
        let field = Field {
            offset: start + index * globals.size,
            bits: globals.size as u8,
            min: globals.min,
            max: globals.max(),
        };
        let page = usage >> 16;
        let number = usage & 0xffff;
        let slot = match usage {
            _ if page == usage::BUTTON => {
                if number == 0 || number > u32::from(MAX_BUTTONS) {
                    continue;
                }
                let slot = &mut found.buttons[number as usize - 1];
                if slot.is_none() {
                    *slot = Some(field);
                }
                None
            }
            usage::X if found.x.is_none() => {
                found.x = Some((field, relative));
                None
            }
            usage::Y if found.y.is_none() => {
                found.y = Some((field, relative));
                None
            }
            // Wheels are counted in detents: only relative ones.
            usage::WHEEL if relative && found.wheel.is_none() => Some(&mut found.wheel),
            usage::AC_PAN if relative && found.pan.is_none() => Some(&mut found.pan),
            _ => continue,
        };
        if let Some(slot) = slot {
            *slot = Some(field);
        }
        found.report_id.get_or_insert(globals.report_id);
    }
}

/// A boot protocol mouse report (HID 1.11 appendix B.2): byte 0 the
/// buttons (bits 0–2: buttons 1–3; the rest are device-specific and kept),
/// bytes 1 and 2 signed X and Y motion, and byte 3, where sent, the wheel
/// (not in the boot format, but sent by most mice and by QEMU).
pub fn boot_report(report: &[u8]) -> Option<Sample> {
    if report.len() < BOOT_REPORT_MIN {
        return None;
    }
    Some(Sample {
        buttons: u32::from(report[0]),
        x: i32::from(report[1] as i8),
        y: i32::from(report[2] as i8),
        wheel: report.get(3).map_or(0, |&wheel| i32::from(wheel as i8)),
        pan: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QEMU's usb-tablet (hw/usb/dev-hid.c): 5 buttons, 16-bit absolute X
    /// and Y in 0..=0x7fff, a relative wheel and AC Pan.
    const TABLET: &[u8] = &[
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x02, // Usage (Mouse)
        0xa1, 0x01, // Collection (Application)
        0x09, 0x01, //   Usage (Pointer)
        0xa1, 0x00, //   Collection (Physical)
        0x05, 0x09, //     Usage Page (Button)
        0x19, 0x01, //     Usage Minimum (1)
        0x29, 0x05, //     Usage Maximum (5)
        0x15, 0x00, //     Logical Minimum (0)
        0x25, 0x01, //     Logical Maximum (1)
        0x95, 0x05, //     Report Count (5)
        0x75, 0x01, //     Report Size (1)
        0x81, 0x02, //     Input (Data, Variable, Absolute)
        0x95, 0x01, //     Report Count (1)
        0x75, 0x03, //     Report Size (3)
        0x81, 0x01, //     Input (Constant)
        0x05, 0x01, //     Usage Page (Generic Desktop)
        0x09, 0x30, //     Usage (X)
        0x09, 0x31, //     Usage (Y)
        0x15, 0x00, //     Logical Minimum (0)
        0x26, 0xff, 0x7f, // Logical Maximum (0x7fff)
        0x35, 0x00, //     Physical Minimum (0)
        0x46, 0xff, 0x7f, // Physical Maximum (0x7fff)
        0x75, 0x10, //     Report Size (16)
        0x95, 0x02, //     Report Count (2)
        0x81, 0x02, //     Input (Data, Variable, Absolute)
        0x05, 0x01, //     Usage Page (Generic Desktop)
        0x09, 0x38, //     Usage (Wheel)
        0x15, 0x81, //     Logical Minimum (-0x7f)
        0x25, 0x7f, //     Logical Maximum (0x7f)
        0x35, 0x00, //     Physical Minimum (same as logical)
        0x45, 0x00, //     Physical Maximum (same as logical)
        0x75, 0x08, //     Report Size (8)
        0x95, 0x01, //     Report Count (1)
        0x81, 0x06, //     Input (Data, Variable, Relative)
        0x05, 0x0c, //     Usage Page (Consumer)
        0x0a, 0x38, 0x02, // Usage (AC Pan)
        0x15, 0x81, //     Logical Minimum (-0x7f)
        0x25, 0x7f, //     Logical Maximum (0x7f)
        0x75, 0x08, //     Report Size (8)
        0x95, 0x01, //     Report Count (1)
        0x81, 0x06, //     Input (Data, Variable, Relative)
        0xc0, //       End Collection
        0xc0, //     End Collection
    ];

    /// A typical mouse with report IDs: a vendor report first, then the
    /// mouse (ID 2): 8 buttons, 12-bit X and Y packed into three bytes, an
    /// 8-bit wheel; the descriptor starts with a keyboard-less consumer
    /// collection and uses PUSH/POP.
    const GAMING_MOUSE: &[u8] = &[
        0x05, 0x0c, // Usage Page (Consumer)
        0x09, 0x01, // Usage (Consumer Control)
        0xa1, 0x01, // Collection (Application)
        0x85, 0x01, //   Report ID (1)
        0x75, 0x10, //   Report Size (16)
        0x95, 0x01, //   Report Count (1)
        0x15, 0x00, //   Logical Minimum (0)
        0x26, 0xff, 0x03, // Logical Maximum (1023)
        0x19, 0x00, //   Usage Minimum (0)
        0x2a, 0xff, 0x03, // Usage Maximum (1023)
        0x81, 0x00, //   Input (Data, Array)
        0xc0, //     End Collection
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x02, // Usage (Mouse)
        0xa1, 0x01, // Collection (Application)
        0x85, 0x02, //   Report ID (2)
        0x09, 0x01, //   Usage (Pointer)
        0xa1, 0x00, //   Collection (Physical)
        0xa4, //         Push
        0x05, 0x09, //     Usage Page (Button)
        0x19, 0x01, //     Usage Minimum (1)
        0x29, 0x08, //     Usage Maximum (8)
        0x15, 0x00, //     Logical Minimum (0)
        0x25, 0x01, //     Logical Maximum (1)
        0x75, 0x01, //     Report Size (1)
        0x95, 0x08, //     Report Count (8)
        0x81, 0x02, //     Input (Data, Variable, Absolute)
        0xb4, //         Pop
        0x05, 0x01, //     Usage Page (Generic Desktop)
        0x16, 0x01, 0xf8, // Logical Minimum (-2047)
        0x26, 0xff, 0x07, // Logical Maximum (2047)
        0x75, 0x0c, //     Report Size (12)
        0x95, 0x02, //     Report Count (2)
        0x09, 0x30, //     Usage (X)
        0x09, 0x31, //     Usage (Y)
        0x81, 0x06, //     Input (Data, Variable, Relative)
        0x15, 0x81, //     Logical Minimum (-127)
        0x25, 0x7f, //     Logical Maximum (127)
        0x75, 0x08, //     Report Size (8)
        0x95, 0x01, //     Report Count (1)
        0x09, 0x38, //     Usage (Wheel)
        0x81, 0x06, //     Input (Data, Variable, Relative)
        0xc0, //       End Collection
        0xc0, //     End Collection
    ];

    /// QEMU's usb-kbd report descriptor, abridged: a keyboard is no
    /// pointer.
    const KEYBOARD: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x75, 0x01, 0x95, 0x08, 0x05, 0x07, 0x19, 0xe0, 0x29,
        0xe7, 0x15, 0x00, 0x25, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x06,
        0x75, 0x08, 0x15, 0x00, 0x25, 0xff, 0x05, 0x07, 0x19, 0x00, 0x29, 0xff, 0x81, 0x00, 0xc0,
    ];

    #[test]
    fn parses_the_qemu_tablet() {
        let layout = Layout::parse(TABLET).unwrap();
        assert!(layout.absolute);
        assert_eq!(layout.report_id, None);
        assert_eq!(layout.button_count(), 5);
        assert_eq!(
            layout.x,
            Field {
                offset: 8,
                bits: 16,
                min: 0,
                max: 0x7fff
            }
        );
        assert_eq!(layout.y.offset, 24);
        assert_eq!(layout.wheel.map(|w| (w.offset, w.min)), Some((40, -127)));
        assert_eq!(layout.pan.map(|p| p.offset), Some(48));
        assert_eq!(
            layout.axes(),
            Axes::Absolute {
                x_max: 0x7fff,
                y_max: 0x7fff
            }
        );
        // Button 2 held, at (0x1234, 0x7fff), wheel down one, pan right.
        let sample = layout
            .decode(&[0b0000_0010, 0x34, 0x12, 0xff, 0x7f, 0xff, 0x01])
            .unwrap();
        assert_eq!(
            sample,
            Sample {
                buttons: 0b10,
                x: 0x1234,
                y: 0x7fff,
                wheel: -1,
                pan: 1,
            }
        );
        // Positions outside the logical range are clamped.
        let sample = layout.decode(&[0, 0xff, 0xff, 0, 0x80]).unwrap();
        assert_eq!((sample.x, sample.y, sample.wheel), (0x7fff, 0x7fff, 0));
        // Too short for Y.
        assert!(layout.decode(&[0, 1, 2, 3]).is_none());
    }

    #[test]
    fn parses_report_ids_push_pop_and_packed_fields() {
        let layout = Layout::parse(GAMING_MOUSE).unwrap();
        assert!(!layout.absolute);
        assert_eq!(layout.report_id, Some(2));
        assert_eq!(layout.button_count(), 8);
        assert_eq!(layout.x.offset, 8);
        assert_eq!(layout.y.offset, 20);
        assert_eq!(layout.x.min, -2047);
        assert_eq!(layout.wheel.map(|w| w.offset), Some(32));
        assert!(layout.pan.is_none());
        assert_eq!(layout.axes(), Axes::Relative);
        // Buttons 1 and 8; X = -2 (0xffe), Y = +3 (0x003); wheel -1.
        let report = [2, 0b1000_0001, 0xfe, 0x3f, 0x00, 0xff];
        assert_eq!(
            layout.decode(&report),
            Some(Sample {
                buttons: 0b1000_0001,
                x: -2,
                y: 3,
                wheel: -1,
                pan: 0,
            })
        );
        // The consumer report (ID 1) is not the pointer's.
        assert!(layout.decode(&[1, 0, 0, 0, 0, 0]).is_none());
        assert!(layout.decode(&[]).is_none());
    }

    #[test]
    fn refuses_what_is_no_pointer_or_malformed() {
        assert_eq!(Layout::parse(KEYBOARD), Err(Error::NotAPointer));
        assert_eq!(Layout::parse(&[]), Err(Error::NotAPointer));
        // Every truncation of a good descriptor is refused or still finds
        // the same pointer; none panics.
        for len in 0..TABLET.len() {
            let parsed = Layout::parse(&TABLET[..len]);
            assert!(parsed.is_err() || parsed == Layout::parse(TABLET));
        }
        // A data item cut short, a pop with nothing pushed, an end
        // without a collection, a 33-bit field.
        assert_eq!(Layout::parse(&[0x26, 0xff]), Err(Error::Malformed));
        assert_eq!(Layout::parse(&[0xb4]), Err(Error::Malformed));
        assert_eq!(Layout::parse(&[0xc0]), Err(Error::Malformed));
        assert_eq!(
            Layout::parse(&[
                0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x75, 33, 0x95, 1, 0x81, 2
            ]),
            Err(Error::Malformed)
        );
        // Reserved item type 3.
        assert_eq!(Layout::parse(&[0x0c]), Err(Error::Malformed));
        // X and Y of different kinds.
        let mixed = [
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01,
            0x09, 0x30, 0x81, 0x06, 0x09, 0x31, 0x81, 0x02, 0xc0,
        ];
        assert_eq!(Layout::parse(&mixed), Err(Error::NotAPointer));
        // Long items are skipped, but must fit.
        assert_eq!(
            Layout::parse(&[0xfe, 0x02, 0x10, 0xaa, 0xbb]),
            Err(Error::NotAPointer)
        );
        assert_eq!(Layout::parse(&[0xfe, 0x05, 0x10]), Err(Error::Malformed));
    }

    #[test]
    fn decodes_boot_reports() {
        assert_eq!(
            boot_report(&[0b101, 0xf6, 5]),
            Some(Sample {
                buttons: 0b101,
                x: -10,
                y: 5,
                wheel: 0,
                pan: 0
            })
        );
        assert_eq!(boot_report(&[1, 1, 1, 0xff]).map(|s| s.wheel), Some(-1));
        assert_eq!(boot_report(&[0, 0]), None);
    }
}
