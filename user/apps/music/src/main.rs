//! Music: your songs and radio stations, played, an app that comes with
//! the system (ADR-0080, ADR-0094, ADR-0135), on the app toolkit.
//!
//! - The songs: `.wav` and `.mp3` files in Home and in Home's `Music`
//!   folder.
//! - Radio: an `http://` or `https://` address of an MP3 stream, typed and
//!   played (Listen), and kept (Save) in `Music/Stations.txt`: a station a
//!   line, `NAME = ADDRESS` or the address alone (`#` starts a comment).
//!   The kept stations follow the songs in the list.
//! - Play and Pause, Stop, the previous and the next; a click on the time
//!   bar goes there (not on the radio, which is live). A song that ends
//!   goes on to the next.
//! - Started with a song's name or a station's address (`app start
//!   app.oceans.music NAME`), it plays that.
//! - The keyboard's media keys (Play/Pause, Stop, Previous, Next) work
//!   whatever has the focus (ADR-0102).
//! - A song or a station is read a piece at a time, never held whole:
//!   `oceans-wav` converts a WAV file's samples, `oceans-mp3` decodes MP3
//!   and converts it, to what the audio service plays.
//!
//! Playing keeps about 500 ms queued ahead of what is heard: each tick
//! (every 100 ms) asks the audio service how much it still holds
//! (`QUEUED`) and tops it up, so the driver's buffer (1.4 s) neither runs
//! dry nor fills, and the window never waits on the sound. The sound
//! device's clock decides, not the app's, which drifts from it. A
//! station's bytes are taken as they arrive, without waiting.
//!
//! Its rights: `window`, `files` (to read the songs, and keep the
//! stations), `sound` (a player end: it plays and cannot record) and
//! `network` (the radio).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use oceans_abi::display::{KEY_NEXT, KEY_PLAY_PAUSE, KEY_PREVIOUS, KEY_STOP};
use oceans_audio_proto::{FRAME, Output};
use oceans_fs_proto::{Kind, MAX_NAME, Node, Shared, flags};
use oceans_mp3::Stream;
use oceans_rt::{Directory, Handle, Start};
use oceans_ui::{Rect, Style, Ui, colour};
use oceans_wav::{OUTPUT_RATE, Wav};
use oceans_web::Response;

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
/// The folder in Home whose songs are listed too, and the stations kept.
const MUSIC: &str = "Music";
const STATIONS: &str = "Music/Stations.txt";
const MAX_STATIONS_FILE: u64 = 64 * 1024;
/// The address field.
const FIELD: u32 = 1;

/// A row of the list.
#[derive(Clone)]
enum Item {
    /// A path from Home.
    Song(String),
    Station {
        name: String,
        address: String,
    },
}

impl Item {
    fn label(&self) -> String {
        match self {
            Self::Song(path) => path.clone(),
            Self::Station { name, .. } => alloc::format!("Radio: {name}"),
        }
    }
}

/// Where the sound comes from.
enum Source {
    Wav {
        file: Node,
        shared: Shared,
        wav: Wav,
    },
    Mp3 {
        file: Node,
        shared: Shared,
        /// The file's size, and where its frames start (after a tag).
        size: u64,
        start: u64,
        /// The next byte to read.
        next: u64,
        stream: Box<Stream>,
    },
    Radio {
        response: Box<Response>,
        stream: Box<Stream>,
    },
}

/// A song or a station being played (or paused).
struct Playing {
    /// Its row, if it has one (an address typed has not).
    index: Option<usize>,
    title: String,
    source: Source,
    /// Output frames handed to the audio service so far (on a song, from
    /// its start).
    queued: u64,
    /// Of those, the frames it had not played yet when last asked
    /// (`QUEUED`): the sound device's own clock, not this app's.
    waiting: u64,
    paused: bool,
}

impl Playing {
    /// Output frames in all: known for a WAV file, estimated from the
    /// bitrate for MP3 once a frame is decoded, none for the radio.
    fn total(&self) -> Option<u64> {
        match &self.source {
            Source::Wav { wav, .. } => Some(wav.output_frames()),
            Source::Mp3 {
                size,
                start,
                stream,
                ..
            } => stream
                .format()
                .map(|format| format.duration_ms(size - start) * u64::from(OUTPUT_RATE) / 1000),
            Source::Radio { .. } => None,
        }
    }

    /// The output frame being heard.
    fn position(&self) -> u64 {
        let at = self.queued.saturating_sub(self.waiting);
        self.total().map_or(at, |total| at.min(total))
    }

    /// Whether everything has been handed over.
    fn drained(&self) -> bool {
        match &self.source {
            Source::Wav { wav, .. } => self.queued >= wav.output_frames(),
            Source::Mp3 { stream, .. } | Source::Radio { stream, .. } => stream.finished(),
        }
    }

    fn close(self) {
        match self.source {
            Source::Wav { file, .. } | Source::Mp3 { file, .. } => file.close(),
            Source::Radio { .. } => {}
        }
    }
}

struct Music {
    home: Option<Node>,
    player: Option<Handle>,
    net: Option<Handle>,
    output: Option<Output>,
    items: Vec<Item>,
    selected: Option<usize>,
    playing: Option<Playing>,
    message: String,
    scanned: bool,
    /// The song or station it was started with (`app start
    /// app.oceans.music NAME`), played once the list is made.
    requested: Option<String>,
    /// Bytes of source frames, read from the file for one `PLAY`.
    source: Vec<u8>,
    /// The address field.
    address: String,
    /// What it last told the desktop it plays (ADR-0103).
    reported: Option<(u8, String)>,
}

fn time(ms: u64) -> String {
    let seconds = ms / 1000;
    alloc::format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Whether a station's `Content-Type` is MPEG audio (or says nothing
/// useful: then the decoder decides).
fn mpeg(kind: &str) -> bool {
    let kind = kind
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        kind.as_str(),
        "audio/mpeg"
            | "audio/mp3"
            | "audio/mpeg3"
            | "audio/x-mpeg"
            | "audio/x-mp3"
            | "audio/mpg"
            | "application/octet-stream"
            | ""
    )
}

/// The name a station gets from its address: the host.
fn host_of(address: &str) -> &str {
    let rest = address
        .strip_prefix("https://")
        .or_else(|| address.strip_prefix("http://"))
        .unwrap_or(address);
    rest.split(['/', '?']).next().unwrap_or(rest)
}

/// Where an MP3 file's frames start: after an ID3v2 tag, if `start` (the
/// file's first bytes) begins with one.
fn after_tag(start: &[u8]) -> u64 {
    match start {
        [b'I', b'D', b'3', _, _, flags, a, b, c, d, ..] => {
            let size = [*a, *b, *c, *d]
                .iter()
                .fold(0u64, |size, &byte| (size << 7) | u64::from(byte & 0x7f));
            // A footer doubles the header.
            10 + size + if flags & 0x10 != 0 { 10 } else { 0 }
        }
        _ => 0,
    }
}

impl Music {
    /// Tells the desktop what plays (ADR-0103) when it changes: what is
    /// playing, or the row selected; playing, paused or stopped.
    fn report(&mut self, ui: &Ui<'_, '_>) {
        use oceans_display_proto::proto::{MAX_NOW_PLAYING, playing};
        let (state, name) = match &self.playing {
            Some(p) if p.paused => (playing::PAUSED, p.title.clone()),
            Some(p) => (playing::PLAYING, p.title.clone()),
            None => (
                playing::STOPPED,
                self.selected
                    .and_then(|i| self.items.get(i))
                    .map_or(String::new(), Item::label),
            ),
        };
        // The name without its folder, cut to what the desktop takes.
        let name = name.rsplit('/').next().unwrap_or(&name);
        let mut title = String::new();
        for c in name.chars().filter(|c| !c.is_control()) {
            if title.len() + c.len_utf8() > MAX_NOW_PLAYING {
                break;
            }
            title.push(c);
        }
        let now = Some((state, title));
        if now != self.reported {
            if let Some((state, title)) = &now {
                ui.now_playing(*state, title);
            }
            self.reported = now;
        }
    }

    /// The songs in Home and in its `Music` folder. (Home is listed
    /// through the handle Core gave: `.` names nothing.)
    fn scan(&mut self, home: &Node, songs: &mut Vec<String>) {
        list(home, "", songs);
        if let Ok((dir, kind)) = home.walk(MUSIC, 0) {
            if kind == Kind::Directory {
                list(&dir, MUSIC, songs);
            }
            dir.close();
        }
    }

    /// The stations kept in `Music/Stations.txt`.
    fn stations(home: &Node) -> Vec<Item> {
        let Ok((file, kind)) = home.walk(STATIONS, 0) else {
            return Vec::new();
        };
        let mut text = Vec::new();
        if kind == Kind::File
            && let Ok(stat) = file.stat()
            && stat.size <= MAX_STATIONS_FILE
        {
            text.resize(stat.size as usize, 0);
            let mut done = 0;
            while done < text.len() {
                match file.read(done as u64, &mut text[done..]) {
                    Ok(0) | Err(_) => break,
                    Ok(got) => done += got,
                }
            }
            text.truncate(done);
        }
        file.close();
        let text = String::from_utf8_lossy(&text);
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| {
                let (name, address) = match line.rsplit_once(" = ") {
                    Some((name, address)) => (name.trim(), address.trim()),
                    None => (host_of(line), line),
                };
                (address.starts_with("http://") || address.starts_with("https://")).then(|| {
                    Item::Station {
                        name: String::from(name),
                        address: String::from(address),
                    }
                })
            })
            .collect()
    }

    fn rescan(&mut self) {
        self.scanned = true;
        self.items.clear();
        if let Some(home) = self.home.take() {
            let mut songs = Vec::new();
            self.scan(&home, &mut songs);
            songs.sort();
            self.items.extend(songs.into_iter().map(Item::Song));
            self.items.extend(Self::stations(&home));
            self.home = Some(home);
        }
        if self.selected.is_none_or(|i| i >= self.items.len()) {
            self.selected = (!self.items.is_empty()).then_some(0);
        }
    }

    /// Keeps `address` as a station, at the end of `Music/Stations.txt`.
    fn save(&mut self, address: &str) {
        let Some(home) = &self.home else {
            return;
        };
        let result = (|| {
            let (dir, _) = home
                .walk(MUSIC, flags::CREATE_DIRECTORY | flags::WRITE)
                .map_err(|e| e.message())?;
            dir.close();
            let (file, _) = home
                .walk(STATIONS, flags::CREATE_FILE | flags::WRITE)
                .map_err(|e| e.message())?;
            let result = (|| {
                let size = file.stat()?.size;
                let line = alloc::format!("{} = {address}\n", host_of(address));
                file.write_all(size, line.as_bytes())?;
                file.sync()
            })();
            file.close();
            result.map_err(|e| e.message())
        })();
        match result {
            Ok(()) => {
                self.message = alloc::format!("Saved: {address}");
                self.rescan();
            }
            Err(why) => self.message = alloc::format!("{STATIONS}: {why}"),
        }
    }

    fn ensure_output(&mut self) -> Result<(), &'static str> {
        if self.output.is_none() {
            let player = self
                .player
                .ok_or("Music may not play sound on this system.")?;
            self.output = Some(
                Output::open(player, OUTPUT_BUFFER).map_err(|_| "the sound output is not there")?,
            );
        }
        Ok(())
    }

    /// Plays row `index` from its start.
    fn start(&mut self, index: usize) {
        let Some(item) = self.items.get(index).cloned() else {
            return;
        };
        self.selected = Some(index);
        match item {
            Item::Song(path) => self.play_song(index, &path),
            Item::Station { name, address } => self.listen(Some(index), &name, &address),
        }
    }

    fn play_song(&mut self, index: usize, path: &str) {
        self.stop();
        match self.open(path) {
            Ok(source) => {
                self.message.clear();
                self.playing = Some(Playing {
                    index: Some(index),
                    title: String::from(path),
                    source,
                    queued: 0,
                    waiting: 0,
                    paused: false,
                });
                self.fill();
            }
            Err(why) => self.message = alloc::format!("{path}: {why}"),
        }
    }

    /// Plays the station at `address`.
    fn listen(&mut self, index: Option<usize>, name: &str, address: &str) {
        self.stop();
        let result = (|| {
            self.ensure_output().map_err(String::from)?;
            let net = self
                .net
                .ok_or_else(|| String::from("Music may not use the network on this system."))?;
            let response = oceans_web::get(net, address).map_err(|p| p.0)?;
            let status = response.head.status;
            if !(200..300).contains(&status) {
                return Err(alloc::format!("HTTP {status} {}", response.head.reason()));
            }
            if let Some(kind) = response.head.content_type()
                && !mpeg(kind)
            {
                return Err(alloc::format!("it sends {kind}: only MP3 stations play"));
            }
            Ok(response)
        })();
        match result {
            Ok(response) => {
                self.message = alloc::format!("Listening to {address}");
                self.playing = Some(Playing {
                    index,
                    title: String::from(name),
                    source: Source::Radio {
                        response: Box::new(response),
                        stream: Stream::new(),
                    },
                    queued: 0,
                    waiting: 0,
                    paused: false,
                });
                self.fill();
            }
            Err(why) => self.message = alloc::format!("{name}: {why}"),
        }
    }

    fn open(&mut self, path: &str) -> Result<Source, &'static str> {
        self.ensure_output()?;
        let home = self.home.as_ref().ok_or("cannot be read")?;
        let (file, _) = home.walk(path, 0).map_err(|_| "cannot be read")?;
        let mp3 = path.to_ascii_lowercase().ends_with(".mp3");
        let header = (|| {
            let size = file.stat().map_err(|_| "cannot be read")?.size;
            let shared = file.attach(READ_BUFFER).map_err(|_| "cannot be read")?;
            let mut start = [0u8; HEADER];
            let len = file
                .read_shared(&shared, 0, &mut start)
                .map_err(|_| "cannot be read")?;
            if mp3 {
                let start = after_tag(&start[..len]).min(size);
                Ok((shared, None, size, start))
            } else {
                Ok((shared, Some(Wav::parse(&start[..len], size)?), size, 0))
            }
        })();
        match header {
            Ok((shared, Some(wav), _, _)) => Ok(Source::Wav { file, shared, wav }),
            Ok((shared, None, size, start)) => Ok(Source::Mp3 {
                file,
                shared,
                size,
                start,
                next: start,
                stream: Stream::new(),
            }),
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
        let target = playing.position() + LEAD_FRAMES;
        let failed = match &mut playing.source {
            Source::Wav { file, shared, wav } => {
                let target = target.min(wav.output_frames());
                fill_wav(
                    file,
                    shared,
                    wav,
                    &mut self.source,
                    output,
                    &mut playing.queued,
                    &mut playing.waiting,
                    target,
                )
            }
            Source::Mp3 {
                file,
                shared,
                next,
                stream,
                ..
            } => {
                let mut feed = |stream: &mut Stream| {
                    while stream.hungry() {
                        let space = stream.space();
                        let len = space.len().min(READ_BUFFER);
                        match file.read_shared(shared, *next, &mut space[..len]) {
                            Ok(0) | Err(_) => {
                                stream.end();
                                break;
                            }
                            Ok(got) => {
                                stream.pushed(got);
                                *next += got as u64;
                            }
                        }
                    }
                    Ok(())
                };
                fill_stream(
                    stream,
                    &mut feed,
                    output,
                    &mut playing.queued,
                    &mut playing.waiting,
                    target,
                )
            }
            Source::Radio { response, stream } => {
                let mut feed = |stream: &mut Stream| {
                    while stream.hungry() {
                        match response.read(stream.space(), 0) {
                            Ok(0) => {
                                if response.ended() {
                                    stream.end();
                                }
                                break;
                            }
                            Ok(got) => stream.pushed(got),
                            Err(problem) => {
                                stream.end();
                                return Err(problem.0);
                            }
                        }
                    }
                    Ok(())
                };
                fill_stream(
                    stream,
                    &mut feed,
                    output,
                    &mut playing.queued,
                    &mut playing.waiting,
                    target,
                )
            }
        };
        if let Some(why) = failed {
            self.message = why;
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
            playing.waiting = 0;
            // (The radio goes on from what comes next.)
            reposition(playing, at);
        }
    }

    fn resume(&mut self) {
        if let Some(playing) = self.playing.as_mut() {
            playing.paused = false;
        }
        self.fill();
    }

    /// Goes to output frame `frame` of the song (not of the radio).
    fn seek(&mut self, frame: u64) {
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        let Some(total) = playing.total() else {
            return;
        };
        if let Some(output) = &self.output {
            let _ = output.stop();
        }
        playing.waiting = 0;
        reposition(playing, frame.min(total));
        self.fill();
    }

    fn stop(&mut self) {
        if let Some(playing) = self.playing.take() {
            if let Some(output) = &self.output {
                let _ = output.stop();
            }
            playing.close();
        }
    }

    /// Each tick: the queue topped up, and the next song when one ends.
    fn tick(&mut self) {
        self.fill();
        let ended = self
            .playing
            .as_ref()
            .filter(|p| !p.paused && p.drained() && p.waiting == 0)
            .map(|p| (p.index, matches!(p.source, Source::Radio { .. })));
        match ended {
            Some((_, true)) => {
                self.stop();
                if self.message.starts_with("Listening") {
                    self.message = String::from("The station stopped.");
                }
            }
            Some((Some(index), false))
                if self
                    .items
                    .get(index + 1)
                    .is_some_and(|item| matches!(item, Item::Song(_))) =>
            {
                self.start(index + 1)
            }
            Some(_) => {
                self.stop();
                self.message = String::from("The end of the list.");
            }
            None => {}
        }
    }
}

/// The `.wav` and `.mp3` files in `dir`, which is `folder` in Home (`""`:
/// Home).
fn list(dir: &Node, folder: &str, songs: &mut Vec<String>) {
    let mut name = [0u8; MAX_NAME];
    for index in 0..MAX_SONGS {
        let Ok(Some((kind, len))) = dir.entry(index, &mut name) else {
            break;
        };
        let Ok(text) = core::str::from_utf8(&name[..len]) else {
            continue;
        };
        let lower = text.to_ascii_lowercase();
        if kind == Kind::File && (lower.ends_with(".wav") || lower.ends_with(".mp3")) {
            songs.push(if folder.is_empty() {
                String::from(text)
            } else {
                alloc::format!("{folder}/{text}")
            });
        }
    }
}

/// Goes on from output frame `frame`: on an MP3 song, from the byte the
/// bitrate puts it at, decoded from there.
fn reposition(playing: &mut Playing, frame: u64) {
    playing.queued = frame;
    if let Source::Mp3 {
        start,
        next,
        size,
        stream,
        ..
    } = &mut playing.source
    {
        let bitrate = stream.format().map_or(0, |f| u64::from(f.bitrate));
        let ms = frame * 1000 / u64::from(OUTPUT_RATE);
        *next = (*start + ms * bitrate / 8).min(*size);
        stream.reset();
    }
}

/// Tops up the audio service from a WAV file. A problem, in words.
#[expect(
    clippy::too_many_arguments,
    reason = "the parts of `Playing` and `Music` it uses, borrowed apart"
)]
fn fill_wav(
    file: &Node,
    shared: &Shared,
    wav: &Wav,
    source: &mut Vec<u8>,
    output: &mut Output,
    queued: &mut u64,
    waiting: &mut u64,
    target: u64,
) -> Option<String> {
    let frame = wav.frame();
    // The output frames whose source frames fit the read buffer (a fast
    // file has more source frames to one output frame).
    let fits = ((READ_BUFFER / frame) as u64 - 2) * u64::from(OUTPUT_RATE) / u64::from(wav.rate);
    while *queued < target {
        let count = ((target - *queued) as usize)
            .min(OUTPUT_BUFFER / FRAME)
            .min(fits as usize);
        let range = wav.source_range(*queued, count);
        let first = *range.start();
        let bytes = (*range.end() - first + 1) as usize * frame;
        source.resize(bytes, 0);
        let offset = wav.data_offset + first * frame as u64;
        let mut done = 0;
        while done < source.len() {
            match file.read_shared(shared, offset + done as u64, &mut source[done..]) {
                Ok(0) | Err(_) => break,
                Ok(got) => done += got,
            }
        }
        let buffer = output.buffer();
        let written = wav.render(
            &source[..done / frame * frame],
            first,
            *queued,
            &mut buffer[..count * FRAME],
        );
        if written == 0 || output.play(0, (written * FRAME) as u32).is_err() {
            *queued = wav.output_frames();
            return Some(String::from("The song could not be played on."));
        }
        *queued += written as u64;
        *waiting += written as u64;
    }
    None
}

/// Tops up the audio service from an MP3 stream, `feed` giving it its
/// bytes. A problem, in words.
fn fill_stream(
    stream: &mut Stream,
    feed: &mut dyn FnMut(&mut Stream) -> Result<(), String>,
    output: &mut Output,
    queued: &mut u64,
    waiting: &mut u64,
    target: u64,
) -> Option<String> {
    while *queued < target {
        if let Err(why) = feed(stream) {
            return Some(why);
        }
        let count = ((target - *queued) as usize).min(OUTPUT_BUFFER / FRAME);
        let buffer = output.buffer();
        let written = stream.render(&mut buffer[..count * FRAME]);
        if written == 0 {
            // Waiting for bytes, or at the end.
            break;
        }
        if output.play(0, (written * FRAME) as u32).is_err() {
            // Nothing more: it has ended.
            stream.reset();
            stream.end();
            return Some(String::from("The sound could not be played on."));
        }
        *queued += written as u64;
        *waiting += written as u64;
    }
    None
}

fn frame(ui: &mut Ui<'_, '_>, music: &mut Music) {
    if !music.scanned {
        // The media keys come here from now on (ADR-0102).
        ui.want_media_keys();
        music.rescan();
        if let Some(name) = music.requested.take() {
            if name.starts_with("http://") || name.starts_with("https://") {
                let host = String::from(host_of(&name));
                music.listen(None, &host, &name);
            } else {
                match music
                    .items
                    .iter()
                    .position(|item| matches!(item, Item::Song(path) if *path == name))
                {
                    Some(index) => music.start(index),
                    None => music.message = alloc::format!("{name}: not among the songs."),
                }
            }
        }
    }
    music.tick();
    music.report(ui);
    ui.background(colour::window());
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
    ui.surface.fill(top, colour::sidebar());
    ui.surface.fill(
        Rect::new(top.x, top.y + top.h - 1, top.w, 1),
        colour::line(),
    );
    let title = match &music.playing {
        Some(playing) => playing.title.clone(),
        None => String::from("Nothing playing"),
    };
    let clip = Rect::new(20, 14, whole.w - 40, 30);
    ui.text_at(20, 16, &title, Style::Title, colour::text(), clip);
    let bar = Rect::new(20, 56, whole.w - 40, 6);
    ui.surface.round_fill(bar, 3, colour::line());
    if let Some(playing) = &music.playing {
        let rate = u64::from(OUTPUT_RATE);
        let at = playing.position();
        let text = match playing.total() {
            Some(total) => {
                let total = total.max(1);
                let filled = (bar.w as u64 * at.min(total) / total) as i32;
                ui.surface.round_fill(
                    Rect::new(bar.x, bar.y, filled.max(6), bar.h),
                    3,
                    colour::accent(),
                );
                alloc::format!("{} / {}", time(at * 1000 / rate), time(total * 1000 / rate))
            }
            // Live: the bar full, and the time listened.
            None => {
                ui.surface.round_fill(bar, 3, colour::accent());
                alloc::format!("Live  {}", time(at * 1000 / rate))
            }
        };
        let width = ui.measure(&text, Style::Body);
        ui.text_at(
            whole.w - 20 - width,
            68,
            &text,
            Style::Body,
            colour::muted(),
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
        if let Some(total) = music.playing.as_ref().and_then(Playing::total) {
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
        let current = music.playing.as_ref().and_then(|p| p.index);
        match text {
            "Pause" => music.pause(),
            "Play" => match (&music.playing, music.selected) {
                (Some(_), _) => music.resume(),
                (None, Some(index)) => music.start(index),
                (None, None) => music.message = String::from("There is nothing to play."),
            },
            "Stop" => music.stop(),
            "Previous" => {
                if let Some(index) = current.or(music.selected) {
                    music.start(index.saturating_sub(1));
                }
            }
            "Next" => {
                if let Some(index) = current.or(music.selected)
                    && index + 1 < music.items.len()
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

    // A station's address: Listen plays it, Save keeps it.
    let row_y = top.y + top.h + 12;
    ui.area = Rect::new(whole.x + 12, row_y, whole.w - 24 - 184, 30);
    ui.y = row_y;
    let mut address = core::mem::take(&mut music.address);
    let entered = ui.text_field(
        FIELD,
        &mut address,
        "A radio station's address (http:// or https://)",
    );
    let typed = String::from(address.trim());
    music.address = address;
    let listen = ui.button_in(Rect::new(whole.w - 12 - 176, row_y, 84, 30), "Listen", true);
    let save = ui.button_in(Rect::new(whole.w - 12 - 84, row_y, 84, 30), "Save", false);
    if (entered || listen || save) && !typed.is_empty() {
        ui.changed = true;
        if !(typed.starts_with("http://") || typed.starts_with("https://")) {
            music.message = String::from("A station's address starts with http:// or https://.");
        } else if save {
            music.save(&typed);
        } else {
            let host = String::from(host_of(&typed));
            music.listen(None, &host, &typed);
        }
    }

    // The songs, then the stations.
    let list_y = row_y + 30 + 12;
    ui.area = Rect::new(whole.x + 12, list_y, whole.w - 24, whole.h - list_y - 24);
    ui.y = ui.area.y;
    if music.items.is_empty() {
        ui.muted("No songs: put .mp3 or .wav files in Home or in Home's Music folder.");
    }
    let labels: Vec<String> = music.items.iter().map(Item::label).collect();
    let names: Vec<&str> = labels.iter().map(String::as_str).collect();
    let current = music.playing.as_ref().and_then(|p| p.index);
    if let Some(index) = ui.list(&names, music.selected) {
        if music.selected == Some(index) || current.is_some() {
            music.start(index);
        } else {
            music.selected = Some(index);
        }
    }
    // What plays: a dot before its name.
    if let Some(index) = current {
        let y = ui.area.y + index as i32 * 34 + 13;
        if y < ui.area.y + ui.area.h {
            ui.surface
                .round_fill(Rect::new(ui.area.x - 6, y, 4, 8), 2, colour::accent());
        }
    }
    if !music.message.is_empty() {
        let message = music.message.clone();
        let status = Rect::new(whole.x, whole.y + whole.h - 26, whole.w, 26);
        ui.surface.fill(status, colour::window());
        ui.text_at(
            16,
            status.y + 4,
            &message,
            Style::Body,
            colour::muted(),
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
        net: directory.find("use", "net"),
        output: None,
        items: Vec::new(),
        selected: None,
        playing: None,
        message: String::new(),
        scanned: false,
        requested: Some(String::from(directory.args().trim())).filter(|name| !name.is_empty()),
        source: Vec::new(),
        address: String::new(),
        reported: None,
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
