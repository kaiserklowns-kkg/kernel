//! hda: the sound driver for Intel High Definition Audio controllers
//! (ADR-0079).
//!
//! An ordinary service. Its authority is what init grants in
//! `services.conf`: the first HD Audio controller (`grant =
//! device-class:040300`), the endpoint it serves (`provide = audio`) and a
//! log. Through the device capability it maps the controller's registers
//! (BAR 0) and allocates DMA memory for the command rings, a buffer
//! descriptor list and a cyclic buffer of samples. It speaks the audio
//! protocol (`oceans-audio-proto`) to clients, copying from their session
//! buffers into the cyclic buffer, so no client sees a physical address.
//!
//! - **Commands** go to codecs through the CORB/RIRB rings, polled.
//! - **The output:** of the first codec with an audio function, the best
//!   pin (a speaker, then line out, then headphones) with a path to a
//!   converter (`oceans_hda::find_output`); every widget on the path is
//!   powered, unmuted at 0 dB and switched to the path.
//! - **One stream,** the first output stream, 48 kHz 16-bit stereo, from
//!   a 64 KiB cyclic buffer (a third of a second). It runs while something
//!   plays; what has not been written yet is silence.
//! - **Polling:** the kernel delivers only MSI-X (ADR-0021) and HD Audio
//!   controllers offer MSI or pin interrupts; the position register is read
//!   while a client waits, sleeping between looks.
//! - **No controller, no codec or no output** (or a failure): the driver
//!   logs it once and keeps running, answering every request with
//!   `IoError`, so init does not restart it in a loop.

#![no_std]
#![no_main]

use core::fmt::Write;
use core::ptr;

use oceans_audio_proto::{Play, Status, op};
use oceans_hda::{
    Capabilities, FORMAT_48K_16_STEREO, Jack, Path, Ring, Widget, WidgetCaps, WidgetType,
    long_verb, param, reg, sd, v, verb,
};
use oceans_rt::{Buffer, Directory, Handle, Start, prot};
use oceans_virtio::Dma;

oceans_rt::entry!(main);

const MAX_SESSIONS: usize = 8;
const MAX_WIDGETS: usize = 64;
/// The cyclic buffer: two halves, a third of a second at 48 kHz stereo.
const RING_SIZE: usize = 64 * 1024;
/// The command page: the command ring (256 entries of 4 bytes), the
/// buffer descriptor list, the response ring (256 entries of 8 bytes).
const PAGE: usize = 4096;
const CORB: usize = 0;
const BDL: usize = 1024;
const RIRB: usize = 2048;
const RING_ENTRIES: u16 = 256;
/// The stream's number on the link (a tag, 1..=15).
const STREAM_TAG: u8 = 1;
const RESET_TIMEOUT_MS: u64 = 100;
const COMMAND_TIMEOUT_MS: u64 = 50;
/// Between looks at the position while a client waits.
const POLL_MS: u64 = 2;

/// Exit codes.
const EXIT_BAD_START: i64 = 2;
const EXIT_RECEIVE: i64 = 3;

/// Command ring registers (not in `oceans_hda::reg`: only this driver's
/// ring handling uses them).
mod rings {
    pub const CORBLBASE: usize = 0x40;
    pub const CORBUBASE: usize = 0x44;
    pub const CORBWP: usize = 0x48;
    pub const CORBRP: usize = 0x4a;
    pub const CORBCTL: usize = 0x4c;
    pub const CORBSIZE: usize = 0x4e;
    pub const RIRBLBASE: usize = 0x50;
    pub const RIRBUBASE: usize = 0x54;
    pub const RIRBWP: usize = 0x58;
    pub const RINTCNT: usize = 0x5a;
    pub const RIRBCTL: usize = 0x5c;
    pub const RIRBSTS: usize = 0x5d;
    pub const RIRBSIZE: usize = 0x5e;
    /// Ring control: the DMA engine runs.
    pub const RUN: u8 = 1 << 1;
    /// Read/write pointer reset bit.
    pub const POINTER_RESET: u16 = 1 << 15;
    /// Size field: 256 entries.
    pub const SIZE_256: u8 = 0b10;
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return EXIT_BAD_START;
    };
    let (Some(log), Some(server)) = (directory.find("log", "log"), directory.find_kind("provide"))
    else {
        return EXIT_BAD_START;
    };
    let Some(device) = directory.find_kind("device") else {
        say(
            log,
            format_args!("hda: no HD Audio controller; requests fail with an I/O error"),
        );
        return serve(log, server, &mut None);
    };
    let mut output = match Sound::start(log, device) {
        Ok(sound) => {
            say(log, format_args!("hda: {}", sound.description.as_str()));
            Some(sound)
        }
        Err(problem) => {
            // Restarting would meet the same controller: say it once and
            // fail requests instead.
            say(
                log,
                format_args!("hda: {problem}; requests fail with an I/O error"),
            );
            None
        }
    };
    serve(log, server, &mut output)
}

fn say(log: Handle, args: core::fmt::Arguments<'_>) {
    let mut line = Buffer::<160>::new();
    let _ = line.write_fmt(args);
    let _ = oceans_rt::debug_write(log, line.as_str());
}

/// Waits up to `timeout_ms` for `done`, sleeping a millisecond between
/// looks.
fn wait_for(timeout_ms: u64, mut done: impl FnMut() -> bool) -> bool {
    let deadline = oceans_rt::clock_ms() + timeout_ms;
    loop {
        if done() {
            return true;
        }
        if oceans_rt::clock_ms() > deadline {
            return false;
        }
        oceans_rt::sleep_ms(1);
    }
}

/// The controller's registers (BAR 0), mapped.
#[derive(Clone, Copy)]
struct Registers {
    base: *mut u8,
    size: usize,
}

// SAFETY (all accessors): `base` maps `size` bytes of the controller's
// register BAR read-write for the life of the process; every access is
// checked against `size`, and HD Audio registers may be accessed at their
// own width (8, 16 or 32 bits), naturally aligned.
impl Registers {
    fn check(self, offset: usize, width: usize) {
        assert!(offset.is_multiple_of(width) && offset + width <= self.size);
    }

    fn read8(self, offset: usize) -> u8 {
        self.check(offset, 1);
        unsafe { ptr::read_volatile(self.base.add(offset)) }
    }

    fn write8(self, offset: usize, value: u8) {
        self.check(offset, 1);
        unsafe { ptr::write_volatile(self.base.add(offset), value) }
    }

    fn read16(self, offset: usize) -> u16 {
        self.check(offset, 2);
        unsafe { ptr::read_volatile(self.base.add(offset).cast()) }
    }

    fn write16(self, offset: usize, value: u16) {
        self.check(offset, 2);
        unsafe { ptr::write_volatile(self.base.add(offset).cast(), value) }
    }

    fn read32(self, offset: usize) -> u32 {
        self.check(offset, 4);
        unsafe { ptr::read_volatile(self.base.add(offset).cast()) }
    }

    fn write32(self, offset: usize, value: u32) {
        self.check(offset, 4);
        unsafe { ptr::write_volatile(self.base.add(offset).cast(), value) }
    }
}

/// The codec link: commands out through the CORB, answers back through the
/// RIRB.
struct Link {
    regs: Registers,
    page: Dma,
    /// The last response read.
    rirb_read: u16,
}

impl Link {
    fn start(regs: Registers, page: Dma) -> Result<Self, &'static str> {
        use rings::*;
        // Both engines stopped before they are set up.
        regs.write8(CORBCTL, 0);
        regs.write8(RIRBCTL, 0);
        if !wait_for(RESET_TIMEOUT_MS, || {
            regs.read8(CORBCTL) & RUN == 0 && regs.read8(RIRBCTL) & RUN == 0
        }) {
            return Err("the command rings do not stop");
        }
        let corb = page.device + CORB as u64;
        let rirb = page.device + RIRB as u64;
        regs.write32(CORBLBASE, corb as u32);
        regs.write32(CORBUBASE, (corb >> 32) as u32);
        regs.write32(RIRBLBASE, rirb as u32);
        regs.write32(RIRBUBASE, (rirb >> 32) as u32);
        regs.write8(CORBSIZE, SIZE_256);
        regs.write8(RIRBSIZE, SIZE_256);
        // The read pointer reset: set, (wait for it,) clear.
        regs.write16(CORBRP, POINTER_RESET);
        wait_for(RESET_TIMEOUT_MS, || {
            regs.read16(CORBRP) & POINTER_RESET != 0
        });
        regs.write16(CORBRP, 0);
        wait_for(RESET_TIMEOUT_MS, || {
            regs.read16(CORBRP) & POINTER_RESET == 0
        });
        regs.write16(CORBWP, 0);
        regs.write16(RIRBWP, POINTER_RESET);
        // A response counter of zero stops some controllers answering; at
        // the count, some stop reading commands until the status is
        // cleared (`command` clears it after every answer).
        regs.write16(RINTCNT, 0xff);
        regs.write8(CORBCTL, RUN);
        regs.write8(RIRBCTL, RUN);
        Ok(Self {
            regs,
            page,
            rirb_read: 0,
        })
    }

    /// Sends one verb and waits for its answer (unsolicited responses are
    /// skipped).
    fn command(&mut self, verb: u32) -> Option<u32> {
        use rings::*;
        let write = (self.regs.read16(CORBWP) % RING_ENTRIES + 1) % RING_ENTRIES;
        self.page.write(CORB + usize::from(write) * 4, verb);
        self.regs.write16(CORBWP, write);
        let deadline = oceans_rt::clock_ms() + COMMAND_TIMEOUT_MS;
        loop {
            let written = self.regs.read16(RIRBWP) % RING_ENTRIES;
            while self.rirb_read != written {
                self.rirb_read = (self.rirb_read + 1) % RING_ENTRIES;
                let at = RIRB + usize::from(self.rirb_read) * 8;
                let answer: u32 = self.page.read(at);
                let extra: u32 = self.page.read(at + 4);
                // The response count and overrun flags, cleared so the
                // controller keeps reading commands.
                self.regs.write8(RIRBSTS, 0x05);
                if extra & (1 << 4) == 0 {
                    return Some(answer);
                }
            }
            if oceans_rt::clock_ms() > deadline {
                return None;
            }
            oceans_rt::sleep_ms(1);
        }
    }

    fn get(&mut self, codec: u8, node: u8, parameter: u8) -> u32 {
        self.command(verb(codec, node, v::GET_PARAMETER, parameter))
            .unwrap_or(0)
    }

    fn set(&mut self, codec: u8, node: u8, id: u16, payload: u8) {
        let _ = self.command(verb(codec, node, id, payload));
    }

    fn set_long(&mut self, codec: u8, node: u8, id: u8, payload: u16) {
        let _ = self.command(long_verb(codec, node, id, payload));
    }
}

/// The sound output: the controller, the chosen path, the running stream.
struct Sound {
    regs: Registers,
    /// The output stream descriptor's registers.
    stream: usize,
    ring_memory: Dma,
    page: Dma,
    ring: Ring,
    running: bool,
    description: Buffer<120>,
}

impl Sound {
    fn start(log: Handle, device: Handle) -> Result<Self, &'static str> {
        oceans_rt::device_enable(device).map_err(|_| "cannot enable the device")?;
        let (memory, size) =
            oceans_rt::device_bar(device, 0).map_err(|_| "cannot get the registers (BAR 0)")?;
        let base = oceans_rt::memory_map(memory, 0, prot::READ | prot::WRITE);
        let _ = oceans_rt::close(memory);
        let regs = Registers {
            base: base.map_err(|_| "cannot map the registers")?,
            size: size as usize,
        };
        if regs.size < 0x100 {
            return Err("the register BAR is too small");
        }
        let caps = Capabilities::decode(regs.read16(reg::GCAP));
        let stream = caps
            .first_output()
            .filter(|&at| at + reg::SD_STRIDE <= regs.size)
            .ok_or("the controller has no output stream")?;

        // Out of reset, and the codecs given time to announce themselves
        // (at least 521 µs).
        regs.write32(reg::GCTL, regs.read32(reg::GCTL) & !1);
        if !wait_for(RESET_TIMEOUT_MS, || regs.read32(reg::GCTL) & 1 == 0) {
            return Err("the controller does not enter reset");
        }
        regs.write32(reg::GCTL, regs.read32(reg::GCTL) | 1);
        if !wait_for(RESET_TIMEOUT_MS, || regs.read32(reg::GCTL) & 1 != 0) {
            return Err("the controller does not leave reset");
        }
        oceans_rt::sleep_ms(2);
        regs.write32(reg::INTCTL, 0);
        let codecs = regs.read16(reg::STATESTS);
        if codecs == 0 {
            return Err("no codec on the link");
        }

        let page = Dma::new(device, PAGE)?;
        let ring_memory = Dma::new(device, RING_SIZE)?;
        let reachable = |dma: &Dma| caps.addressing_64 || dma.device + dma.len as u64 <= 1 << 32;
        if !reachable(&page) || !reachable(&ring_memory) {
            return Err("DMA memory lies beyond the controller's 32-bit addresses");
        }
        let mut link = Link::start(regs, page)?;

        // The first codec with an audio function and an output path.
        let mut chosen = None;
        for codec in (0..15u8).filter(|c| codecs & (1 << c) != 0) {
            if let Some(found) = configure(&mut link, codec) {
                chosen = Some(found);
                break;
            }
        }
        let Some((codec, path, vendor)) = chosen else {
            return Err("no codec with an output (speaker, line out, headphones)");
        };
        let _ = log;

        // The converter takes the stream.
        let converter = path.converter();
        link.set_long(codec, converter, v::SET_FORMAT, FORMAT_48K_16_STEREO);
        link.set(codec, converter, v::SET_STREAM_CHANNEL, STREAM_TAG << 4);

        let mut description = Buffer::<120>::new();
        let _ = write!(
            description,
            "Intel HD Audio {}.{}, codec {:04x}:{:04x}, {} (pin {:#x}, converter {:#x}), 48 kHz stereo",
            regs.read8(reg::VMAJ),
            regs.read8(reg::VMIN),
            vendor >> 16,
            vendor & 0xffff,
            path.jack.name(),
            path.pin(),
            converter
        );
        let mut sound = Self {
            regs,
            stream,
            ring_memory,
            page,
            ring: Ring::new(RING_SIZE as u32),
            running: false,
            description,
        };
        sound.program_stream()?;
        Ok(sound)
    }

    fn sd_read32(&self, register: usize) -> u32 {
        self.regs.read32(self.stream + register)
    }

    fn sd_write32(&self, register: usize, value: u32) {
        self.regs.write32(self.stream + register, value);
    }

    /// Resets the stream and sets it up: the buffer descriptor list (two
    /// halves of the cyclic buffer), the format, the tag. Not running.
    fn program_stream(&mut self) -> Result<(), &'static str> {
        let control = self.stream + sd::CTL;
        self.regs.write32(control, sd::CTL_RESET);
        if !wait_for(RESET_TIMEOUT_MS, || {
            self.regs.read32(control) & sd::CTL_RESET != 0
        }) {
            return Err("the output stream does not reset");
        }
        self.regs.write32(control, 0);
        if !wait_for(RESET_TIMEOUT_MS, || {
            self.regs.read32(control) & sd::CTL_RESET == 0
        }) {
            return Err("the output stream does not leave reset");
        }
        let half = (RING_SIZE / 2) as u32;
        for (i, offset) in [0u64, u64::from(half)].into_iter().enumerate() {
            let entry = oceans_hda::bdl_entry(self.ring_memory.device + offset, half, false);
            self.page.copy_in(BDL + i * 16, &entry);
        }
        let bdl = self.page.device + BDL as u64;
        self.sd_write32(sd::BDPL, bdl as u32);
        self.sd_write32(sd::BDPU, (bdl >> 32) as u32);
        self.sd_write32(sd::CBL, RING_SIZE as u32);
        self.regs.write16(self.stream + sd::LVI, 1);
        self.regs
            .write16(self.stream + sd::FMT, FORMAT_48K_16_STEREO);
        self.sd_write32(sd::CTL, oceans_hda::stream_control(STREAM_TAG));
        self.ring = Ring::new(RING_SIZE as u32);
        self.silence();
        self.running = false;
        Ok(())
    }

    /// Everything not yet written is silence.
    fn silence(&mut self) {
        let free = self.ring.free() as usize;
        let start = self.ring.write_offset();
        let first = free.min(RING_SIZE - start);
        // SAFETY: both pieces lie inside the cyclic buffer's mapping.
        unsafe {
            ptr::write_bytes(self.ring_memory.virt.add(start), 0, first);
            ptr::write_bytes(self.ring_memory.virt, 0, free - first);
        }
    }

    fn update(&mut self) {
        if self.running {
            let position = self.sd_read32(sd::LPIB);
            self.ring.advance(position);
        }
    }

    fn run(&mut self) {
        if !self.running {
            let control = self.sd_read32(sd::CTL);
            self.sd_write32(sd::CTL, control | sd::CTL_RUN);
            self.running = true;
        }
    }

    fn rest(&mut self) -> Status {
        if self.running {
            let control = self.sd_read32(sd::CTL);
            self.sd_write32(sd::CTL, control & !sd::CTL_RUN);
            wait_for(RESET_TIMEOUT_MS, || {
                self.sd_read32(sd::CTL) & sd::CTL_RUN == 0
            });
        }
        // From the start again: the position resets with the stream.
        match self.program_stream() {
            Ok(()) => Status::Ok,
            Err(_) => Status::IoError,
        }
    }

    /// `PLAY`: copies `bytes` into the cyclic buffer, waiting while it is
    /// full.
    fn play(&mut self, bytes: &[u8]) -> Status {
        let mut done = 0;
        while done < bytes.len() {
            self.update();
            let free = self.ring.free() as usize;
            if free == 0 {
                // Full: it must be running to make room.
                self.run();
                oceans_rt::sleep_ms(POLL_MS);
                continue;
            }
            let at = self.ring.write_offset();
            let take = free.min(bytes.len() - done).min(RING_SIZE - at);
            self.ring_memory.copy_in(at, &bytes[done..done + take]);
            self.ring.written += take as u64;
            done += take;
        }
        // What comes after is silence until more is played.
        self.silence();
        self.run();
        Status::Ok
    }

    /// `DRAIN`: waits until all of it has been played, then rests.
    fn drain(&mut self) -> Status {
        let left = self.ring.written - self.ring.played;
        let timeout = left * 1000 / u64::from(oceans_hda::BYTES_PER_SECOND) + 1000;
        let deadline = oceans_rt::clock_ms() + timeout;
        while self.running && !self.ring.drained() {
            if oceans_rt::clock_ms() > deadline {
                // The position stopped moving: the device is stuck.
                self.rest();
                return Status::IoError;
            }
            oceans_rt::sleep_ms(POLL_MS);
            self.update();
        }
        self.rest()
    }
}

/// Finds codec `codec`'s audio function and its best output path, and sets
/// the path up. Returns the codec, the path and its vendor/device id.
fn configure(link: &mut Link, codec: u8) -> Option<(u8, Path, u32)> {
    let vendor = link.get(codec, 0, param::VENDOR);
    let groups = oceans_hda::node_range(link.get(codec, 0, param::NODE_COUNT));
    let function = groups
        .clone()
        .find(|&node| oceans_hda::is_audio_function(link.get(codec, node, param::FUNCTION_TYPE)))?;
    link.set(codec, function, v::SET_POWER_STATE, 0);
    let mut widgets = [Widget {
        node: 0,
        caps: WidgetCaps::decode(0),
        jack: Jack::None,
        can_output: false,
        inputs: [0; 8],
        input_count: 0,
    }; MAX_WIDGETS];
    let mut count = 0;
    for node in oceans_hda::node_range(link.get(codec, function, param::NODE_COUNT)) {
        if count == MAX_WIDGETS {
            break;
        }
        let caps = WidgetCaps::decode(link.get(codec, node, param::WIDGET_CAPS));
        let mut widget = Widget {
            node,
            caps,
            jack: Jack::None,
            can_output: false,
            inputs: [0; 8],
            input_count: 0,
        };
        if caps.kind == WidgetType::Pin {
            widget.jack = Jack::decode(
                link.command(verb(codec, node, v::GET_CONFIG_DEFAULT, 0))
                    .unwrap_or(0),
            );
            widget.can_output = oceans_hda::pin_can_output(link.get(codec, node, param::PIN_CAPS));
        }
        if caps.connections {
            let (length, long) =
                oceans_hda::connection_length(link.get(codec, node, param::CONNECTION_LENGTH));
            // Long-form lists (16-bit entries) are rare: such widgets get
            // no inputs here.
            if !long {
                let mut index = 0;
                while index < length.min(8) {
                    let answer = link
                        .command(verb(codec, node, v::GET_CONNECTION_LIST, index))
                        .unwrap_or(0);
                    for entry in oceans_hda::connections(answer) {
                        if index < length.min(8) {
                            widget.inputs[usize::from(index)] = entry;
                            index += 1;
                        }
                    }
                }
                widget.input_count = length.min(8);
            }
        }
        widgets[count] = widget;
        count += 1;
    }
    let widgets = &widgets[..count];
    let path = oceans_hda::find_output(widgets)?;

    // Power, route and unmute every widget on the path.
    for (i, &node) in path.nodes().iter().enumerate() {
        link.set(codec, node, v::SET_POWER_STATE, 0);
        let Some(widget) = widgets.iter().find(|w| w.node == node) else {
            continue;
        };
        let gain = match oceans_hda::amp_zero_db(link.get(codec, node, param::OUTPUT_AMP_CAPS)) {
            0 => oceans_hda::amp_zero_db(link.get(codec, function, param::OUTPUT_AMP_CAPS)),
            gain => gain,
        };
        if widget.caps.output_amp {
            link.set_long(
                codec,
                node,
                v::SET_AMP,
                oceans_hda::amp_payload(true, 0, gain),
            );
        }
        if i + 1 < path.nodes().len() {
            let select = path.selects[i];
            match widget.caps.kind {
                WidgetType::Mixer => {
                    if widget.caps.input_amp {
                        link.set_long(
                            codec,
                            node,
                            v::SET_AMP,
                            oceans_hda::amp_payload(false, select, gain),
                        );
                    }
                }
                _ if widget.input_count > 1 => {
                    link.set(codec, node, v::SET_CONNECTION_SELECT, select);
                }
                _ => {}
            }
        }
    }
    let pin = path.pin();
    link.set(
        codec,
        pin,
        v::SET_PIN_CONTROL,
        oceans_hda::pin_output_control(path.jack),
    );
    // An external amplifier, where the pin controls one.
    link.set(codec, pin, v::SET_EAPD, 0x02);
    Some((codec, path, vendor))
}

/// Serves the audio protocol until the endpoint fails; without a sound
/// output, every request is answered with `IoError`.
fn serve(log: Handle, server: Handle, sound: &mut Option<Sound>) -> i64 {
    let mut sessions: [Option<Session>; MAX_SESSIONS] = [None; MAX_SESSIONS];
    let mut next_badge = 1;
    let mut data = [0u8; 64];
    let mut handles = [Handle(0); 4];
    loop {
        let got = match oceans_rt::ipc_receive_msg(server, &mut data, &mut handles) {
            Ok(got) => got,
            Err(error) => {
                say(log, format_args!("hda: receive failed: {error:?}"));
                return EXIT_RECEIVE;
            }
        };
        let find = |badge| {
            sessions
                .iter()
                .position(|s| s.is_some_and(|s: Session| s.badge == badge))
        };
        if got.closed {
            if let Some(index) = find(got.badge)
                && let Some(session) = sessions[index].take()
            {
                let _ = oceans_rt::memory_unmap(session.buffer.cast_mut());
            }
            continue;
        }
        let received = &handles[..got.handles_len];
        let session = find(got.badge).and_then(|i| sessions[i]);
        let mut reply_handle = None;
        let mut text = Buffer::<120>::new();
        let status = match (sound.as_mut(), got.label, session) {
            (None, _, _) => Status::IoError,
            (Some(sound), op::INFO, _) => {
                let _ = text.write_str(sound.description.as_str());
                Status::Ok
            }
            (Some(_), op::OPEN, None) if got.badge == 0 && received.len() == 1 => {
                match open_session(server, received[0], next_badge, &mut sessions) {
                    Ok(handle) => {
                        next_badge += 1;
                        reply_handle = Some(handle);
                        Status::Ok
                    }
                    Err(status) => status,
                }
            }
            (Some(sound), op::PLAY, Some(session)) => {
                match Play::decode(&data[..got.data_len]).and_then(|p| p.checked(session.size)) {
                    Some(range) => {
                        // SAFETY: `range` lies inside the session's buffer,
                        // mapped for as long as the session is open; the
                        // client does not write it during the call.
                        let bytes = unsafe {
                            core::slice::from_raw_parts(
                                session.buffer.add(range.start),
                                range.len(),
                            )
                        };
                        sound.play(bytes)
                    }
                    None => Status::OutOfRange,
                }
            }
            (Some(sound), op::DRAIN, Some(_)) => sound.drain(),
            (Some(sound), op::STOP, Some(_)) => sound.rest(),
            _ => Status::BadRequest,
        };
        // Capabilities sent with a request we did not take are closed.
        if !matches!(got.label, op::OPEN) || status != Status::Ok {
            for &handle in received {
                let _ = oceans_rt::close(handle);
            }
        }
        let reply: &[Handle] = match &reply_handle {
            Some(handle) => core::slice::from_ref(handle),
            None => &[],
        };
        if oceans_rt::ipc_reply_msg(status as u64, text.as_str().as_bytes(), reply).is_err()
            && let Some(handle) = reply_handle
        {
            let _ = oceans_rt::close(handle);
        }
    }
}

#[derive(Clone, Copy)]
struct Session {
    badge: u64,
    buffer: *const u8,
    size: usize,
}

/// Maps a client's buffer and mints its session handle.
fn open_session(
    server: Handle,
    memory: Handle,
    badge: u64,
    sessions: &mut [Option<Session>; MAX_SESSIONS],
) -> Result<Handle, Status> {
    let slot = sessions
        .iter()
        .position(Option::is_none)
        .ok_or(Status::NoSpace)?;
    let size = oceans_rt::memory_size(memory).map_err(|_| Status::BadRequest)? as usize;
    if size == 0 || size > oceans_audio_proto::MAX_BUFFER {
        return Err(Status::BadRequest);
    }
    let buffer = oceans_rt::memory_map(memory, 0, prot::READ).map_err(|_| Status::BadRequest)?;
    // The mapping keeps the memory; the handle is not needed.
    let _ = oceans_rt::close(memory);
    match oceans_rt::endpoint_mint(server, badge) {
        Ok(handle) => {
            sessions[slot] = Some(Session {
                badge,
                buffer: buffer.cast_const(),
                size,
            });
            Ok(handle)
        }
        Err(_) => {
            let _ = oceans_rt::memory_unmap(buffer);
            Err(Status::NoSpace)
        }
    }
}
