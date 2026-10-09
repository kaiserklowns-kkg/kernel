//! Music: the WAV files in the user's files, played, an app that comes
//! with the system (ADR-0080, ADR-0094), on the app toolkit.
//!
//! - The songs: `.wav` files in Home and in Home's `Music` folder.
//! - Play and Pause, Stop, the previous and the next song; a click on the
//!   time bar goes there. A song that ends goes on to the next.
//! - Started with a song's name (`app start app.oceans.music NAME`), it
//!   plays that song.
//! - The keyboard's media keys (Play/Pause, Stop, Previous, Next) work
//!   whatever has the focus (ADR-0102).
//! - A song is read a piece at a time (`oceans-wav` converts it to what
//!   the audio service plays), never held whole.
//!
//! Playing keeps about 500 ms queued ahead of what is heard: each tick
//! (every 100 ms) asks the audio service how much it still holds
//! (`QUEUED`) and tops it up, so the driver's buffer (1.4 s) neither runs
//! dry nor fills, and the window never waits on the sound. The sound
//! device's clock decides, not the app's, which drifts from it.
//!
//! Its rights: `window`, `files` (to read the songs) and `sound` (a player
//! end: it plays and cannot record).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use oceans_abi::display::{KEY_NEXT, KEY_PLAY_PAUSE, KEY_PREVIOUS, KEY_STOP};
use oceans_audio_proto::{FRAME, Output};
use oceans_fs_proto::{Kind, MAX_NAME, Node, Shared};
use oceans_rt::{Directory, Handle, Start};
use oceans_ui::{Rect, Style, Ui, colour};
use oceans_wav::{OUTPUT_RATE, Wav};

oceans_rt::entry!(main);

const WIDTH: u16 = 640;
const HEIGHT: u16 = 440;
/// How often the queue is topped up, and the window redrawn.
const TICK_MS: u64 = 100;
/// How far ahead of what is heard the queue runs: half a second, so a
/// late tick (a busy window) does not let it run dry; well inside the
/// driver's 1.4 s buffer, so `PLAY` never waits.
const LEAD_FRAMES: u64 = OUTPUT_RATE as u64 / 2;
/// The session buffer: up to 8192 output frames a `PLAY`.
const OUTPUT_BUFFER: usize = 32 * 1024;
/// The file reads' shared buffer: the source frames of one `PLAY`.
const READ_BUFFER: usize = 64 * 1024;
/// How much of a file's start holds its header.
const HEADER: usize = 4096;
const MAX_SONGS: u32 = 300;
/// The folder in Home whose songs are listed too.
const MUSIC: &str = "Music";

/// A song being played (or paused).
struct Playing {
    index: usize,
    file: Node,
    shared: Shared,
    wav: Wav,
    /// Output frames handed to the audio service so far.
    queued: u64,
    /// Of those, the frames it had not played yet when last asked
    /// (`QUEUED`): the sound device's own clock, not this app's.
    waiting: u64,
    paused: bool,
}

impl Playing {
    /// The output frame being heard.
    fn position(&self) -> u64 {
        self.queued
            .saturating_sub(self.waiting)
            .min(self.wav.output_frames())
    }
}

struct Music {
    home: Option<Node>,
    player: Option<Handle>,
    output: Option<Output>,
    /// Paths from Home.
    songs: Vec<String>,
    selected: Option<usize>,
    playing: Option<Playing>,
    message: String,
    scanned: bool,
    /// The song it was started with (`app start app.oceans.music NAME`),
    /// played once the songs are found.
    requested: Option<String>,
    /// Bytes of source frames, read from the file for one `PLAY`.
    source: Vec<u8>,
}

fn time(ms: u64) -> String {
    let seconds = ms / 1000;
    alloc::format!("{}:{:02}", seconds / 60, seconds % 60)
}

impl Music {
    /// The `.wav` files in Home and in its `Music` folder. (Home is listed
    /// through the handle Core gave: `.` names nothing.)
    fn scan(&mut self, home: &Node) {
        self.list(home, "");
        if let Ok((dir, kind)) = home.walk(MUSIC, 0) {
            if kind == Kind::Directory {
                self.list(&dir, MUSIC);
            }
            dir.close();
        }
    }

    /// The `.wav` files in `dir`, which is `folder` in Home (`""`: Home).
    fn list(&mut self, dir: &Node, folder: &str) {
        let mut name = [0u8; MAX_NAME];
        for index in 0..MAX_SONGS {
            let Ok(Some((kind, len))) = dir.entry(index, &mut name) else {
                break;
            };
            let Ok(text) = core::str::from_utf8(&name[..len]) else {
                continue;
            };
            if kind == Kind::File && text.to_ascii_lowercase().ends_with(".wav") {
                self.songs.push(if folder.is_empty() {
                    String::from(text)
                } else {
                    alloc::format!("{folder}/{text}")
                });
            }
        }
    }

    fn rescan(&mut self) {
        self.scanned = true;
        self.songs.clear();
        if let Some(home) = self.home.take() {
            self.scan(&home);
            self.home = Some(home);
        }
        self.songs.sort();
        if self.selected.is_none() && !self.songs.is_empty() {
            self.selected = Some(0);
        }
    }

    /// Starts song `index` from the start.
    fn start(&mut self, index: usize) {
        self.stop();
        let Some(path) = self.songs.get(index).cloned() else {
            return;
        };
        self.selected = Some(index);
        match self.open(&path) {
            Ok((file, shared, wav)) => {
                self.message.clear();
                self.playing = Some(Playing {
                    index,
                    file,
                    shared,
                    wav,
                    queued: 0,
                    waiting: 0,
                    paused: false,
                });
                self.fill();
            }
            Err(why) => self.message = alloc::format!("{path}: {why}"),
        }
    }

    fn open(&mut self, path: &str) -> Result<(Node, Shared, Wav), &'static str> {
        if self.output.is_none() {
            let player = self
                .player
                .ok_or("Music may not play sound on this system.")?;
            self.output = Some(
                Output::open(player, OUTPUT_BUFFER).map_err(|_| "the sound output is not there")?,
            );
        }
        let home = self.home.as_ref().ok_or("cannot be read")?;
        let (file, _) = home.walk(path, 0).map_err(|_| "cannot be read")?;
        let header = (|| {
            let size = file.stat().map_err(|_| "cannot be read")?.size;
            let shared = file.attach(READ_BUFFER).map_err(|_| "cannot be read")?;
            let mut start = [0u8; HEADER];
            let len = file
                .read_shared(&shared, 0, &mut start)
                .map_err(|_| "cannot be read")?;
            let wav = Wav::parse(&start[..len], size)?;
            Ok((shared, wav))
        })();
        match header {
            Ok((shared, wav)) => Ok((file, shared, wav)),
            Err(why) => {
                file.close();
                Err(why)
            }
        }
    }

    /// Asks how much the audio service still holds, and tops it up to
    /// `LEAD_FRAMES`.
    fn fill(&mut self) {
        let (Some(playing), Some(output)) = (self.playing.as_mut(), self.output.as_mut()) else {
            return;
        };
        if playing.paused {
            return;
        }
        playing.waiting = match output.queued() {
            Ok(bytes) => u64::from(bytes) / FRAME as u64,
            Err(_) => {
                self.message = String::from("The sound output went away.");
                playing.paused = true;
                return;
            }
        };
        let total = playing.wav.output_frames();
        let target = (playing.position() + LEAD_FRAMES).min(total);
        let frame = playing.wav.frame();
        // The output frames whose source frames fit the read buffer (a
        // fast file has more source frames to one output frame).
        let fits = ((READ_BUFFER / frame) as u64 - 2) * u64::from(OUTPUT_RATE)
            / u64::from(playing.wav.rate);
        while playing.queued < target {
            let count = ((target - playing.queued) as usize)
                .min(OUTPUT_BUFFER / FRAME)
                .min(fits as usize);
            let range = playing.wav.source_range(playing.queued, count);
            let first = *range.start();
            let bytes = (*range.end() - first + 1) as usize * frame;
            self.source.resize(bytes, 0);
            let offset = playing.wav.data_offset + first * frame as u64;
            let mut done = 0;
            while done < self.source.len() {
                match playing.file.read_shared(
                    &playing.shared,
                    offset + done as u64,
                    &mut self.source[done..],
                ) {
                    Ok(0) | Err(_) => break,
                    Ok(got) => done += got,
                }
            }
            let source = &self.source[..done / frame * frame];
            let buffer = output.buffer();
            let written =
                playing
                    .wav
                    .render(source, first, playing.queued, &mut buffer[..count * FRAME]);
            if written == 0 || output.play(0, (written * FRAME) as u32).is_err() {
                self.message = String::from("The song could not be played on.");
                playing.queued = total;
                break;
            }
            playing.queued += written as u64;
            playing.waiting += written as u64;
        }
    }

    fn pause(&mut self) {
        if let (Some(playing), Some(output)) = (self.playing.as_mut(), self.output.as_ref()) {
            // Where it is, then what is queued dropped: it goes on from
            // there.
            if let Ok(bytes) = output.queued() {
                playing.waiting = u64::from(bytes) / FRAME as u64;
            }
            let at = playing.position();
            let _ = output.stop();
            playing.paused = true;
            playing.queued = at;
            playing.waiting = 0;
        }
    }

    fn resume(&mut self) {
        if let Some(playing) = self.playing.as_mut() {
            playing.paused = false;
        }
        self.fill();
    }

    /// Goes to output frame `frame` of the song.
    fn seek(&mut self, frame: u64) {
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        if let Some(output) = &self.output {
            let _ = output.stop();
        }
        playing.queued = frame.min(playing.wav.output_frames());
        playing.waiting = 0;
        self.fill();
    }

    fn stop(&mut self) {
        if let Some(playing) = self.playing.take() {
            if let Some(output) = &self.output {
                let _ = output.stop();
            }
            playing.file.close();
        }
    }

    /// Each tick: the queue topped up, and the next song when one ends.
    fn tick(&mut self) {
        self.fill();
        let ended = self.playing.as_ref().and_then(|p| {
            let total = p.wav.output_frames();
            (!p.paused && p.queued >= total && p.waiting == 0).then_some(p.index)
        });
        match ended {
            Some(index) if index + 1 < self.songs.len() => self.start(index + 1),
            Some(_) => {
                self.stop();
                self.message = String::from("The end of the list.");
            }
            None => {}
        }
    }
}

fn frame(ui: &mut Ui<'_, '_>, music: &mut Music) {
    if !music.scanned {
        // The media keys come here from now on (ADR-0102).
        ui.want_media_keys();
        music.rescan();
        if let Some(name) = music.requested.take() {
            match music.songs.iter().position(|song| *song == name) {
                Some(index) => music.start(index),
                None => music.message = alloc::format!("{name}: not among the songs."),
            }
        }
    }
    music.tick();
    ui.background(colour::WINDOW);
    let whole = ui.area;
    if music.home.is_none() {
        ui.area = Rect::new(whole.x + 20, whole.y + 20, whole.w - 40, whole.h - 40);
        ui.y = ui.area.y;
        ui.heading("Music");
        ui.muted("Music may not open your files on this system.");
        return;
    }

    // The player at the top: what plays, the time bar, the buttons.
    let top = Rect::new(whole.x, whole.y, whole.w, 132);
    ui.surface.fill(top, colour::SIDEBAR);
    ui.surface
        .fill(Rect::new(top.x, top.y + top.h - 1, top.w, 1), colour::LINE);
    let title = match &music.playing {
        Some(playing) => music.songs[playing.index].clone(),
        None => String::from("Nothing playing"),
    };
    let clip = Rect::new(20, 14, whole.w - 40, 30);
    ui.text_at(20, 16, &title, Style::Title, colour::TEXT, clip);
    let bar = Rect::new(20, 56, whole.w - 40, 6);
    ui.surface.round_fill(bar, 3, colour::LINE);
    if let Some(playing) = &music.playing {
        let total = playing.wav.output_frames().max(1);
        let at = playing.position();
        let filled = (bar.w as u64 * at / total) as i32;
        ui.surface.round_fill(
            Rect::new(bar.x, bar.y, filled.max(6), bar.h),
            3,
            colour::ACCENT,
        );
        let rate = u64::from(OUTPUT_RATE);
        let text = alloc::format!("{} / {}", time(at * 1000 / rate), time(total * 1000 / rate));
        let width = ui.measure(&text, Style::Body);
        ui.text_at(
            whole.w - 20 - width,
            68,
            &text,
            Style::Body,
            colour::MUTED,
            top,
        );
    }
    // A click on the time bar (a little taller than drawn) goes there.
    let target = Rect::new(bar.x, bar.y - 6, bar.w, bar.h + 12);
    if let Some((x, y)) = ui.input.click
        && target.contains(x, y)
    {
        ui.input.click = None;
        ui.changed = true;
        if let Some(total) = music.playing.as_ref().map(|p| p.wav.output_frames()) {
            let frame = total * (x - bar.x).max(0) as u64 / bar.w.max(1) as u64;
            music.seek(frame);
        }
    }
    let playing_now = music.playing.as_ref().is_some_and(|p| !p.paused);
    let main_label = if playing_now { "Pause" } else { "Play" };
    let buttons = [
        ("Previous", 20, 96),
        (main_label, 124, 96),
        ("Stop", 228, 80),
        ("Next", 316, 80),
    ];
    let mut action = None;
    for (text, x, w) in buttons {
        if ui.button_in(Rect::new(x, 88, w, 30), text, x == 124) {
            action = Some(text);
        }
    }
    // The media keys (ADR-0102), whatever has the focus: Music asked for
    // them. Other keys are left to the widgets.
    let mut others = Vec::new();
    for key in core::mem::take(&mut ui.input.keys) {
        match key {
            KEY_PLAY_PAUSE => action = Some(main_label),
            KEY_STOP => action = Some("Stop"),
            KEY_PREVIOUS => action = Some("Previous"),
            KEY_NEXT => action = Some("Next"),
            other => others.push(other),
        }
    }
    ui.input.keys = others;
    if let Some(text) = action {
        ui.changed = true;
        let current = music.playing.as_ref().map(|p| p.index);
        match text {
            "Pause" => music.pause(),
            "Play" => match (&music.playing, music.selected) {
                (Some(_), _) => music.resume(),
                (None, Some(index)) => music.start(index),
                (None, None) => music.message = String::from("There are no songs to play."),
            },
            "Stop" => music.stop(),
            "Previous" => {
                if let Some(index) = current.or(music.selected) {
                    music.start(index.saturating_sub(1));
                }
            }
            "Next" => {
                if let Some(index) = current.or(music.selected)
                    && index + 1 < music.songs.len()
                {
                    music.start(index + 1);
                }
            }
            _ => {}
        }
    }
    if ui.button_in(Rect::new(whole.w - 112, 88, 92, 30), "Refresh", false) {
        music.rescan();
    }

    // The songs.
    ui.area = Rect::new(
        whole.x + 12,
        top.y + top.h + 12,
        whole.w - 24,
        whole.h - top.h - 24,
    );
    ui.y = ui.area.y;
    if music.songs.is_empty() {
        ui.muted("No songs: put .wav files in Home or in Home's Music folder.");
    }
    let names: Vec<&str> = music.songs.iter().map(String::as_str).collect();
    let current = music.playing.as_ref().map(|p| p.index);
    if let Some(index) = ui.list(&names, music.selected) {
        if music.selected == Some(index) || current.is_some() {
            music.start(index);
        } else {
            music.selected = Some(index);
        }
    }
    // The song playing: a dot before its name.
    if let Some(index) = current {
        let y = ui.area.y + index as i32 * 34 + 13;
        if y < ui.area.y + ui.area.h {
            ui.surface
                .round_fill(Rect::new(ui.area.x - 6, y, 4, 8), 2, colour::ACCENT);
        }
    }
    if !music.message.is_empty() {
        let message = music.message.clone();
        let status = Rect::new(whole.x, whole.y + whole.h - 26, whole.w, 26);
        ui.surface.fill(status, colour::WINDOW);
        ui.text_at(
            16,
            status.y + 4,
            &message,
            Style::Body,
            colour::MUTED,
            status,
        );
    }
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return oceans_ui::EXIT_BAD_START;
    };
    let mut music = Music {
        home: directory.find("use", "files").map(Node),
        player: directory.find("use", "audio"),
        output: None,
        songs: Vec::new(),
        selected: None,
        playing: None,
        message: String::new(),
        scanned: false,
        requested: Some(String::from(directory.args().trim())).filter(|name| !name.is_empty()),
        source: Vec::new(),
    };
    let code = oceans_ui::run_ticking(
        &directory,
        "",
        WIDTH,
        HEIGHT,
        Some(TICK_MS),
        &mut music,
        frame,
    );
    music.stop();
    code
}
