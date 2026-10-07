//! hda: the sound driver for Intel High Definition Audio controllers
//! (ADR-0079, ADR-0087).
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
//! - **The input** (ADR-0087): of the first codec with one, the best pin
//!   (a microphone, then line in) reached from an input converter
//!   (`oceans_hda::find_input`), set up the same way; the first input
//!   stream, in the same format, into a 256 KiB cyclic buffer (1.4 s). It
//!   runs from a capture session's first `RECORD` until `STOP` or the
//!   session closes; one capture session at a time.
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

use oceans_audio_proto::{Play, Status, op, open_flags};
use oceans_hda::{
    Capabilities, Capture, FORMAT_48K_16_STEREO, Jack, Path, Ring, Widget, WidgetCaps, WidgetType,
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
/// The input stream's buffer descriptor list (128-byte aligned).
const INPUT_BDL: usize = 1152;
const RIRB: usize = 2048;
/// The input's cyclic buffer: 1.4 s at 48 kHz stereo, so a client taking
/// it in pieces has time between them (ADR-0087).
const CAPTURE_SIZE: usize = 256 * 1024;
const RING_ENTRIES: u16 = 256;
/// The streams' numbers on the link (tags, 1..=15).
const STREAM_TAG: u8 = 1;
const INPUT_TAG: u8 = 2;
/// An input whose position does not move for this long is stuck.
const STALL_MS: u64 = 1000;
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

/// The sound output (and input): the controller, the chosen paths, the
/// streams.
struct Sound {
    log: Handle,
    regs: Registers,
    /// The output stream descriptor's registers.
    stream: usize,
    ring_memory: Dma,
    page: Dma,
    ring: Ring,
    running: bool,
    description: Buffer<120>,
    input: Option<Input>,
}

/// The input stream (ADR-0087).
struct Input {
    /// Its stream descriptor's registers.
    stream: usize,
    memory: Dma,
    capture: Capture,
    running: bool,
    /// The capture session's badge.
    owner: Option<u64>,
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

        // The first codec with an audio function and an output path, and
        // the first with an input path (often the same).
        let mut chosen = None;
        let mut input_path = None;
        for codec in (0..15u8).filter(|c| codecs & (1 << c) != 0) {
            let Some(found) = read_codec(&mut link, codec) else {
                continue;
            };
            if chosen.is_none()
                && let Some(path) = configure_output(&mut link, &found)
            {
                chosen = Some((codec, path, found.vendor));
            }
            if input_path.is_none()
                && let Some(path) = configure_input(&mut link, &found)
            {
                input_path = Some((codec, path, found.vendor));
            }
        }
        let Some((codec, path, vendor)) = chosen else {
            return Err("no codec with an output (speaker, line out, headphones)");
        };

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
        let input = match (caps.first_input(), input_path) {
            (Some(at), Some((codec, path, vendor))) if at + reg::SD_STRIDE <= regs.size => {
                let converter = path.nodes()[0];
                link.set_long(codec, converter, v::SET_FORMAT, FORMAT_48K_16_STEREO);
                link.set(codec, converter, v::SET_STREAM_CHANNEL, INPUT_TAG << 4);
                let memory = Dma::new(device, CAPTURE_SIZE)?;
                if !reachable(&memory) {
                    return Err("DMA memory lies beyond the controller's 32-bit addresses");
                }
                let mut description = Buffer::<120>::new();
                let _ = write!(
                    description,
                    "{} (pin {:#x}, converter {:#x}), codec {:04x}:{:04x}, 48 kHz stereo",
                    path.jack.name(),
                    path.nodes()[usize::from(path.len) - 1],
                    converter,
                    vendor >> 16,
                    vendor & 0xffff
                );
                Some(Input {
                    stream: at,
                    memory,
                    capture: Capture::new(CAPTURE_SIZE as u32),
                    running: false,
                    owner: None,
                    description,
                })
            }
            _ => None,
        };
        let mut sound = Self {
            log,
            regs,
            stream,
            ring_memory,
            page,
            ring: Ring::new(RING_SIZE as u32),
            running: false,
            description,
            input,
        };
        sound.program_stream()?;
        match &sound.input {
            Some(input) => say(
                log,
                format_args!("hda: input: {}", input.description.as_str()),
            ),
            None => say(log, format_args!("hda: no input (microphone, line in)")),
        }
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
        program_descriptor(
            self.regs,
            &self.page,
            self.stream,
            BDL,
            &self.ring_memory,
            STREAM_TAG,
        )?;
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

    /// `RECORD`: fills `out` with what comes in next, starting the input
    /// if it is not running (ADR-0087).
    fn record(&mut self, out: &mut [u8]) -> Status {
        let regs = self.regs;
        let Some(input) = self.input.as_mut() else {
            return Status::IoError;
        };
        if !input.running {
            if input.program(regs, &self.page).is_err() {
                return Status::IoError;
            }
            let control = regs.read32(input.stream + sd::CTL);
            regs.write32(input.stream + sd::CTL, control | sd::CTL_RUN);
            input.running = true;
        }
        let mut done = 0;
        let mut progress = oceans_rt::clock_ms();
        while done < out.len() {
            input.capture.advance(regs.read32(input.stream + sd::LPIB));
            let available = input.capture.available() as usize;
            if available == 0 {
                if oceans_rt::clock_ms() - progress > STALL_MS {
                    // The position stopped moving: the device is stuck.
                    say(self.log, format_args!("hda: the input stopped moving"));
                    input.stop(regs, &self.page);
                    return Status::IoError;
                }
                oceans_rt::sleep_ms(POLL_MS);
                continue;
            }
            progress = oceans_rt::clock_ms();
            let at = input.capture.read_offset();
            let take = available.min(out.len() - done).min(CAPTURE_SIZE - at);
            input.memory.copy_out(at, &mut out[done..done + take]);
            input.capture.taken += take as u64;
            done += take;
        }
        Status::Ok
    }

    /// The capture session `badge` stopped or went away: the input stops.
    fn stop_input(&mut self, badge: u64) -> Status {
        let (regs, log) = (self.regs, self.log);
        match self.input.as_mut() {
            Some(input) if input.owner == Some(badge) => {
                if input.capture.lost > 0 {
                    say(
                        log,
                        format_args!(
                            "hda: {} bytes of input were not taken in time and dropped",
                            input.capture.lost
                        ),
                    );
                }
                input.owner = None;
                input.stop(regs, &self.page)
            }
            _ => Status::BadRequest,
        }
    }
}

impl Input {
    /// Resets the input stream and sets it up, not running; the count
    /// starts again.
    fn program(&mut self, regs: Registers, page: &Dma) -> Result<(), &'static str> {
        program_descriptor(regs, page, self.stream, INPUT_BDL, &self.memory, INPUT_TAG)?;
        self.capture = Capture::new(CAPTURE_SIZE as u32);
        self.running = false;
        Ok(())
    }

    fn stop(&mut self, regs: Registers, page: &Dma) -> Status {
        if self.running {
            let control = regs.read32(self.stream + sd::CTL);
            regs.write32(self.stream + sd::CTL, control & !sd::CTL_RUN);
            wait_for(RESET_TIMEOUT_MS, || {
                regs.read32(self.stream + sd::CTL) & sd::CTL_RUN == 0
            });
        }
        match self.program(regs, page) {
            Ok(()) => Status::Ok,
            Err(_) => Status::IoError,
        }
    }
}

/// Resets the stream descriptor at `at` and sets it up, not running: a
/// buffer descriptor list (at `bdl` in the command page) of two halves of
/// `memory`, the format, the tag.
fn program_descriptor(
    regs: Registers,
    page: &Dma,
    at: usize,
    bdl: usize,
    memory: &Dma,
    tag: u8,
) -> Result<(), &'static str> {
    let control = at + sd::CTL;
    regs.write32(control, sd::CTL_RESET);
    if !wait_for(RESET_TIMEOUT_MS, || {
        regs.read32(control) & sd::CTL_RESET != 0
    }) {
        return Err("a stream does not reset");
    }
    regs.write32(control, 0);
    if !wait_for(RESET_TIMEOUT_MS, || {
        regs.read32(control) & sd::CTL_RESET == 0
    }) {
        return Err("a stream does not leave reset");
    }
    let half = (memory.len / 2) as u32;
    for (i, offset) in [0u64, u64::from(half)].into_iter().enumerate() {
        let entry = oceans_hda::bdl_entry(memory.device + offset, half, false);
        page.copy_in(bdl + i * 16, &entry);
    }
    let list = page.device + bdl as u64;
    regs.write32(at + sd::BDPL, list as u32);
    regs.write32(at + sd::BDPU, (list >> 32) as u32);
    regs.write32(at + sd::CBL, 2 * half);
    regs.write16(at + sd::LVI, 1);
    regs.write16(at + sd::FMT, FORMAT_48K_16_STEREO);
    regs.write32(at + sd::CTL, oceans_hda::stream_control(tag));
    Ok(())
}

/// A codec's audio function and its widgets.
struct Codec {
    codec: u8,
    function: u8,
    vendor: u32,
    widgets: [Widget; MAX_WIDGETS],
    count: usize,
}

impl Codec {
    fn widgets(&self) -> &[Widget] {
        &self.widgets[..self.count]
    }

    fn widget(&self, node: u8) -> Option<&Widget> {
        self.widgets().iter().find(|w| w.node == node)
    }
}

/// Finds codec `codec`'s audio function (powered on) and reads its widgets.
fn read_codec(link: &mut Link, codec: u8) -> Option<Codec> {
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
        can_input: false,
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
            can_input: false,
            inputs: [0; 8],
            input_count: 0,
        };
        if caps.kind == WidgetType::Pin {
            widget.jack = Jack::decode(
                link.command(verb(codec, node, v::GET_CONFIG_DEFAULT, 0))
                    .unwrap_or(0),
            );
            let pin_caps = link.get(codec, node, param::PIN_CAPS);
            widget.can_output = oceans_hda::pin_can_output(pin_caps);
            widget.can_input = oceans_hda::pin_can_input(pin_caps);
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
    Some(Codec {
        codec,
        function,
        vendor,
        widgets,
        count,
    })
}

/// The 0 dB gain step of `node`'s amp (`caps`: which amp's parameter),
/// or the function group's when the node has none of its own.
fn zero_db(link: &mut Link, codec: &Codec, node: u8, caps: u8) -> u8 {
    match oceans_hda::amp_zero_db(link.get(codec.codec, node, caps)) {
        0 => oceans_hda::amp_zero_db(link.get(codec.codec, codec.function, caps)),
        gain => gain,
    }
}

/// The codec's best output path, set up: every widget powered, routed and
/// unmuted at 0 dB, the pin driving its jack.
fn configure_output(link: &mut Link, found: &Codec) -> Option<Path> {
    let (codec, widgets) = (found.codec, found.widgets());
    let path = oceans_hda::find_output(widgets)?;

    // Power, route and unmute every widget on the path.
    for (i, &node) in path.nodes().iter().enumerate() {
        link.set(codec, node, v::SET_POWER_STATE, 0);
        let Some(widget) = widgets.iter().find(|w| w.node == node) else {
            continue;
        };
        let gain = zero_db(link, found, node, param::OUTPUT_AMP_CAPS);
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
    Some(path)
}

/// The codec's best input path (ADR-0087), set up: every widget powered,
/// routed, its input amps unmuted at 0 dB, the pin taking its input (a
/// microphone with its bias voltage).
fn configure_input(link: &mut Link, found: &Codec) -> Option<Path> {
    let codec = found.codec;
    let path = oceans_hda::find_input(found.widgets())?;
    let nodes = path.nodes();
    for (i, &node) in nodes.iter().enumerate() {
        link.set(codec, node, v::SET_POWER_STATE, 0);
        let Some(widget) = found.widget(node) else {
            continue;
        };
        let gain = zero_db(link, found, node, param::INPUT_AMP_CAPS);
        if i + 1 < nodes.len() {
            // Where the sound comes from: the next node, input `select`.
            let select = path.selects[i];
            if widget.caps.input_amp {
                let index = if widget.caps.kind == WidgetType::Mixer {
                    select
                } else {
                    0
                };
                link.set_long(
                    codec,
                    node,
                    v::SET_AMP,
                    oceans_hda::amp_payload(false, index, gain),
                );
            }
            if widget.caps.kind != WidgetType::Mixer && widget.input_count > 1 {
                link.set(codec, node, v::SET_CONNECTION_SELECT, select);
            }
        } else if widget.caps.input_amp {
            // The pin's own amp (a microphone boost): 0 dB.
            link.set_long(
                codec,
                node,
                v::SET_AMP,
                oceans_hda::amp_payload(false, 0, gain),
            );
        }
        if widget.caps.output_amp && i > 0 {
            let gain = zero_db(link, found, node, param::OUTPUT_AMP_CAPS);
            link.set_long(
                codec,
                node,
                v::SET_AMP,
                oceans_hda::amp_payload(true, 0, gain),
            );
        }
    }
    let pin = nodes[nodes.len() - 1];
    let pin_caps = link.get(codec, pin, param::PIN_CAPS);
    link.set(
        codec,
        pin,
        v::SET_PIN_CONTROL,
        oceans_hda::pin_input_control(path.jack, pin_caps),
    );
    Some(path)
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
                if session.capture
                    && let Some(sound) = sound.as_mut()
                {
                    sound.stop_input(session.badge);
                }
                let _ = oceans_rt::memory_unmap(session.buffer);
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
            (Some(sound), op::INPUT_INFO, _) => match &sound.input {
                Some(input) => {
                    let _ = text.write_str(input.description.as_str());
                    Status::Ok
                }
                None => Status::IoError,
            },
            (Some(sound), op::OPEN, None) if got.badge == 0 && received.len() == 1 => {
                let capture = match &data[..got.data_len] {
                    [] => Some(false),
                    [open_flags::CAPTURE] => Some(true),
                    _ => None,
                };
                let input = sound.input.as_mut();
                match (capture, input) {
                    (None, _) => Status::BadRequest,
                    (Some(true), None) => Status::IoError,
                    (Some(true), Some(input)) if input.owner.is_some() => Status::Busy,
                    (Some(capture), input) => {
                        match open_session(server, received[0], next_badge, capture, &mut sessions)
                        {
                            Ok(handle) => {
                                if capture && let Some(input) = input {
                                    input.owner = Some(next_badge);
                                }
                                next_badge += 1;
                                reply_handle = Some(handle);
                                Status::Ok
                            }
                            Err(status) => status,
                        }
                    }
                }
            }
            (Some(sound), op::RECORD, Some(session)) if session.capture => {
                match Play::decode(&data[..got.data_len]).and_then(|p| p.checked(session.size)) {
                    Some(range) => {
                        // SAFETY: `range` lies inside the session's buffer,
                        // mapped writable for as long as the session is
                        // open; the client does not touch it during the
                        // call.
                        let bytes = unsafe {
                            core::slice::from_raw_parts_mut(
                                session.buffer.add(range.start),
                                range.len(),
                            )
                        };
                        sound.record(bytes)
                    }
                    None => Status::OutOfRange,
                }
            }
            (Some(sound), op::STOP, Some(session)) if session.capture => {
                sound.stop_input(session.badge)
            }
            (Some(sound), op::PLAY, Some(session)) if !session.capture => {
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
            (Some(sound), op::DRAIN, Some(session)) if !session.capture => sound.drain(),
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
    buffer: *mut u8,
    size: usize,
    /// A capture session (ADR-0087): its buffer is mapped writable.
    capture: bool,
}

/// Maps a client's buffer (writable for a capture session) and mints its
/// session handle.
fn open_session(
    server: Handle,
    memory: Handle,
    badge: u64,
    capture: bool,
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
    let access = if capture {
        prot::READ | prot::WRITE
    } else {
        prot::READ
    };
    let buffer = oceans_rt::memory_map(memory, 0, access).map_err(|_| Status::BadRequest)?;
    // The mapping keeps the memory; the handle is not needed.
    let _ = oceans_rt::close(memory);
    match oceans_rt::endpoint_mint(server, badge) {
        Ok(handle) => {
            sessions[slot] = Some(Session {
                badge,
                buffer,
                size,
                capture,
            });
            Ok(handle)
        }
        Err(_) => {
            let _ = oceans_rt::memory_unmap(buffer);
            Err(Status::NoSpace)
        }
    }
}
