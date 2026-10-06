//! Intel High Definition Audio (ADR-0079), host-tested: the controller's
//! registers, the codec commands ("verbs") and what their answers mean,
//! the search for a path from a converter to an output jack, stream
//! formats and buffer descriptor lists. The driver (`user/hda`) moves the
//! bytes; the decisions are made here.
//!
//! HD Audio (2004) is the sound controller of nearly every PC since: one
//! controller on PCI (class 04, subclass 03), and codecs on its link,
//! each a graph of widgets (converters, mixers, selectors, pins).

#![no_std]

/// Controller registers (offsets in BAR 0).
pub mod reg {
    /// Global capabilities (u16): streams and 64-bit addressing.
    pub const GCAP: usize = 0x00;
    pub const VMIN: usize = 0x02;
    pub const VMAJ: usize = 0x03;
    /// Global control (u32): bit 0 takes the controller out of reset.
    pub const GCTL: usize = 0x08;
    /// Codecs that answered after reset (u16), one bit each.
    pub const STATESTS: usize = 0x0e;
    pub const INTCTL: usize = 0x20;
    /// The immediate command interface: command out, response in, status.
    pub const ICOI: usize = 0x60;
    pub const ICII: usize = 0x64;
    pub const ICIS: usize = 0x68;
    /// The first stream descriptor; each takes `SD_STRIDE` bytes.
    pub const SD_BASE: usize = 0x80;
    pub const SD_STRIDE: usize = 0x20;
}

/// Stream descriptor registers (offsets in a descriptor).
pub mod sd {
    /// Control (3 bytes): reset, run, the stream's number in bits 23:20.
    pub const CTL: usize = 0x00;
    pub const STS: usize = 0x03;
    /// The position in the cyclic buffer (u32).
    pub const LPIB: usize = 0x04;
    /// The cyclic buffer's length (u32).
    pub const CBL: usize = 0x08;
    /// The last valid index of the buffer descriptor list (u16).
    pub const LVI: usize = 0x0c;
    pub const FMT: usize = 0x12;
    pub const BDPL: usize = 0x18;
    pub const BDPU: usize = 0x1c;

    pub const CTL_RESET: u32 = 1 << 0;
    pub const CTL_RUN: u32 = 1 << 1;
}

/// `ICIS` bits.
pub mod icis {
    /// A command is being sent.
    pub const BUSY: u32 = 1 << 0;
    /// A response is waiting in `ICII`.
    pub const VALID: u32 = 1 << 1;
}

/// The global capabilities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub output_streams: u8,
    pub input_streams: u8,
    pub addressing_64: bool,
}

impl Capabilities {
    pub fn decode(gcap: u16) -> Self {
        Self {
            output_streams: ((gcap >> 12) & 0xf) as u8,
            input_streams: ((gcap >> 8) & 0xf) as u8,
            addressing_64: gcap & 1 != 0,
        }
    }

    /// The register offset of the first output stream's descriptor (input
    /// streams come first).
    pub fn first_output(&self) -> Option<usize> {
        (self.output_streams > 0)
            .then(|| reg::SD_BASE + usize::from(self.input_streams) * reg::SD_STRIDE)
    }
}

// ---- Codec commands ------------------------------------------------------

/// A verb with a 12-bit identifier and an 8-bit payload.
pub fn verb(codec: u8, node: u8, id: u16, payload: u8) -> u32 {
    (u32::from(codec & 0xf) << 28)
        | (u32::from(node) << 20)
        | (u32::from(id & 0xfff) << 8)
        | u32::from(payload)
}

/// A verb with a 4-bit identifier and a 16-bit payload (formats, amps).
pub fn long_verb(codec: u8, node: u8, id: u8, payload: u16) -> u32 {
    (u32::from(codec & 0xf) << 28)
        | (u32::from(node) << 20)
        | (u32::from(id & 0xf) << 16)
        | u32::from(payload)
}

/// Verb identifiers.
pub mod v {
    pub const GET_PARAMETER: u16 = 0xf00;
    pub const GET_CONNECTION_LIST: u16 = 0xf02;
    pub const SET_CONNECTION_SELECT: u16 = 0x701;
    pub const SET_POWER_STATE: u16 = 0x705;
    pub const SET_STREAM_CHANNEL: u16 = 0x706;
    pub const SET_PIN_CONTROL: u16 = 0x707;
    pub const SET_EAPD: u16 = 0x70c;
    pub const GET_CONFIG_DEFAULT: u16 = 0xf1c;
    /// 4-bit verbs.
    pub const SET_FORMAT: u8 = 0x2;
    pub const SET_AMP: u8 = 0x3;
}

/// `GET_PARAMETER` parameters.
pub mod param {
    pub const VENDOR: u8 = 0x00;
    pub const NODE_COUNT: u8 = 0x04;
    pub const FUNCTION_TYPE: u8 = 0x05;
    pub const WIDGET_CAPS: u8 = 0x09;
    pub const PIN_CAPS: u8 = 0x0c;
    pub const CONNECTION_LENGTH: u8 = 0x0e;
    pub const OUTPUT_AMP_CAPS: u8 = 0x12;
}

/// `NODE_COUNT`: the first node and how many.
pub fn node_range(answer: u32) -> core::ops::Range<u8> {
    let start = ((answer >> 16) & 0xff) as u8;
    let count = (answer & 0xff) as u8;
    start..start.saturating_add(count)
}

/// `FUNCTION_TYPE`: an audio function group.
pub fn is_audio_function(answer: u32) -> bool {
    answer & 0xff == 1
}

/// What a widget is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WidgetType {
    /// A digital-to-analogue converter: where a stream's samples go in.
    Output,
    Input,
    Mixer,
    Selector,
    /// A jack (or a speaker).
    Pin,
    Other,
}

/// `WIDGET_CAPS`, as far as finding a path needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WidgetCaps {
    pub kind: WidgetType,
    pub output_amp: bool,
    pub input_amp: bool,
    pub connections: bool,
}

impl WidgetCaps {
    pub fn decode(answer: u32) -> Self {
        Self {
            kind: match (answer >> 20) & 0xf {
                0 => WidgetType::Output,
                1 => WidgetType::Input,
                2 => WidgetType::Mixer,
                3 => WidgetType::Selector,
                4 => WidgetType::Pin,
                _ => WidgetType::Other,
            },
            input_amp: answer & (1 << 1) != 0,
            output_amp: answer & (1 << 2) != 0,
            connections: answer & (1 << 8) != 0,
        }
    }
}

/// What a pin is wired to, from its configuration default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Jack {
    LineOut,
    Speaker,
    Headphone,
    /// Inputs and the rest.
    Other,
    /// Nothing is connected.
    None,
}

impl Jack {
    pub fn decode(config_default: u32) -> Self {
        if config_default >> 30 == 1 {
            return Self::None;
        }
        match (config_default >> 20) & 0xf {
            0 => Self::LineOut,
            1 => Self::Speaker,
            2 => Self::Headphone,
            _ => Self::Other,
        }
    }

    /// Which output to prefer: a built-in speaker, then line out, then
    /// headphones (lower is better).
    pub fn preference(self) -> Option<u8> {
        match self {
            Self::Speaker => Some(0),
            Self::LineOut => Some(1),
            Self::Headphone => Some(2),
            Self::Other | Self::None => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::LineOut => "line out",
            Self::Speaker => "speaker",
            Self::Headphone => "headphones",
            Self::Other => "other",
            Self::None => "not connected",
        }
    }
}

/// `PIN_CAPS`: the pin can drive an output.
pub fn pin_can_output(answer: u32) -> bool {
    answer & (1 << 4) != 0
}

/// `CONNECTION_LENGTH`: how many, and whether entries are long (16-bit).
pub fn connection_length(answer: u32) -> (u8, bool) {
    ((answer & 0x7f) as u8, answer & (1 << 7) != 0)
}

/// One `GET_CONNECTION_LIST` answer (short entries): up to four nodes.
pub fn connections(answer: u32) -> [u8; 4] {
    answer.to_le_bytes()
}

/// `OUTPUT_AMP_CAPS`: the gain step that means 0 dB (the offset).
pub fn amp_zero_db(answer: u32) -> u8 {
    (answer & 0x7f) as u8
}

/// `SET_AMP`'s payload: output (or input) amp, both channels, unmuted, at
/// `gain` (input amps: of connection `index`).
pub fn amp_payload(output: bool, index: u8, gain: u8) -> u16 {
    let direction = if output { 1 << 15 } else { 1 << 14 };
    direction | (1 << 13) | (1 << 12) | (u16::from(index & 0xf) << 8) | u16::from(gain & 0x7f)
}

/// Pin control: drive the output (and the headphone amplifier).
pub fn pin_output_control(jack: Jack) -> u8 {
    0x40 | if jack == Jack::Headphone { 0x80 } else { 0 }
}

/// The format Oceans plays: 48 kHz, 16 bits, two channels.
pub const FORMAT_48K_16_STEREO: u16 = (0b001 << 4) | 1;
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// Bytes per second of the format.
pub const BYTES_PER_SECOND: u32 = SAMPLE_RATE * CHANNELS * 2;

// ---- Finding a path -----------------------------------------------------

/// A widget as the path search sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Widget {
    pub node: u8,
    pub caps: WidgetCaps,
    /// For pins: what it is wired to and whether it can drive an output.
    pub jack: Jack,
    pub can_output: bool,
    /// Its inputs, in connection list order.
    pub inputs: [u8; 8],
    pub input_count: u8,
}

impl Widget {
    pub fn inputs(&self) -> &[u8] {
        &self.inputs[..usize::from(self.input_count.min(8))]
    }
}

/// The way from a converter to a jack: the nodes from the pin back to the
/// converter, and at each step which of the node's inputs leads on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Path {
    pub nodes: [u8; 6],
    /// `selects[i]`: the index of `nodes[i + 1]` among `nodes[i]`'s inputs.
    pub selects: [u8; 6],
    pub len: u8,
    pub jack: Jack,
}

impl Path {
    pub fn nodes(&self) -> &[u8] {
        &self.nodes[..usize::from(self.len)]
    }

    /// The converter at the end.
    pub fn converter(&self) -> u8 {
        self.nodes[usize::from(self.len) - 1]
    }

    pub fn pin(&self) -> u8 {
        self.nodes[0]
    }
}

/// The best output: among pins that can drive an output and are wired to
/// a speaker, line out or headphones (in that order of preference), the
/// first with a path of at most six widgets to a converter.
pub fn find_output(widgets: &[Widget]) -> Option<Path> {
    let find = |node: u8| widgets.iter().find(|w| w.node == node);
    let mut pins: [Option<&Widget>; 32] = [None; 32];
    let mut count = 0;
    for w in widgets {
        if w.caps.kind == WidgetType::Pin
            && w.can_output
            && w.jack.preference().is_some()
            && count < pins.len()
        {
            pins[count] = Some(w);
            count += 1;
        }
    }
    let pins = &mut pins[..count];
    pins.sort_unstable_by_key(|w| w.map(|w| (w.jack.preference(), w.node)));
    for pin in pins.iter().flatten() {
        let mut path = Path {
            nodes: [0; 6],
            selects: [0; 6],
            len: 1,
            jack: pin.jack,
        };
        path.nodes[0] = pin.node;
        if search(&find, &mut path) {
            return Some(path);
        }
    }
    None
}

/// Depth first from the path's last node towards a converter.
fn search<'a>(find: &impl Fn(u8) -> Option<&'a Widget>, path: &mut Path) -> bool {
    let len = usize::from(path.len);
    let Some(here) = find(path.nodes[len - 1]) else {
        return false;
    };
    if here.caps.kind == WidgetType::Output {
        return true;
    }
    if len == path.nodes.len() {
        return false;
    }
    for (index, &next) in here.inputs().iter().enumerate() {
        if path.nodes().contains(&next) {
            continue;
        }
        let Some(widget) = find(next) else {
            continue;
        };
        if !matches!(
            widget.caps.kind,
            WidgetType::Output | WidgetType::Mixer | WidgetType::Selector
        ) {
            continue;
        }
        path.nodes[len] = next;
        path.selects[len - 1] = index as u8;
        path.len += 1;
        if search(find, path) {
            return true;
        }
        path.len -= 1;
    }
    false
}

// ---- Buffer descriptor lists ----------------------------------------------

/// One entry of a buffer descriptor list (16 bytes): where a piece of the
/// cyclic buffer is, how long, and whether to interrupt after it.
pub fn bdl_entry(address: u64, length: u32, interrupt: bool) -> [u8; 16] {
    let mut entry = [0u8; 16];
    entry[..8].copy_from_slice(&address.to_le_bytes());
    entry[8..12].copy_from_slice(&length.to_le_bytes());
    entry[12..].copy_from_slice(&u32::from(interrupt).to_le_bytes());
    entry
}

/// The stream number (tag) in a descriptor's control register.
pub fn stream_control(tag: u8) -> u32 {
    u32::from(tag & 0xf) << 20
}

// ---- The cyclic buffer ----------------------------------------------------

/// The driver's view of the cyclic buffer: how much was written in and how
/// much the controller has played, as running totals, from its position
/// register (which wraps at the buffer's length).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ring {
    pub size: u64,
    pub written: u64,
    pub played: u64,
    last_position: u32,
}

impl Ring {
    pub fn new(size: u32) -> Self {
        Self {
            size: u64::from(size),
            ..Self::default()
        }
    }

    /// The controller's position moved to `position`.
    pub fn advance(&mut self, position: u32) {
        let size = self.size as u32;
        let position = position % size.max(1);
        let moved = (position + size - self.last_position) % size.max(1);
        self.last_position = position;
        self.played += u64::from(moved);
        // Starved: it played past what was written (silence was there).
        if self.played > self.written {
            self.written = self.played;
        }
    }

    /// Bytes that can be written now without overwriting what is still to
    /// be played.
    pub fn free(&self) -> u64 {
        self.size - (self.written - self.played)
    }

    /// Where the next byte goes in the buffer.
    pub fn write_offset(&self) -> usize {
        (self.written % self.size) as usize
    }

    /// Everything written has been played.
    pub fn drained(&self) -> bool {
        self.played >= self.written
    }
}

#[cfg(test)]
mod tests;
