//! Image Viewer: PNG and BMP pictures in the user's files (`/home`), an app
//! that comes with the system (ADR-0090), on the app toolkit and
//! `oceans-image`.
//!
//! - A picture's name (in Home; `folder/name.png` for one in a folder) and
//!   Open, or the name given when the app is started
//!   (`app start app.oceans.viewer NAME`).
//! - Previous and Next (and Page Up, Page Down) go through the pictures in
//!   the same folder, by name.
//! - Fit (the whole picture, shrunk to the window, never enlarged) or
//!   Actual size, where the arrows move around a picture larger than the
//!   window. Transparency shows over a checkerboard.
//!
//! Its rights: `window` and `files` (the user's files, nothing else).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use oceans_abi::display::{KEY_DOWN, KEY_LEFT, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_RIGHT, KEY_UP};
use oceans_fs_proto::{Kind, MAX_NAME, Node};
use oceans_image::{Format, Image};
use oceans_rt::{Directory, Start};
use oceans_ui::{Rect, Style, Ui, colour};

oceans_rt::entry!(main);

const WIDTH: u16 = 760;
const HEIGHT: u16 = 560;
/// The smallest the window may be resized to (ADR-0097).
const MIN_SIZE: (u16, u16) = (480, 320);
const TOOLBAR: i32 = 52;
const STATUS: i32 = 28;
/// The keyboard's owners: the name field and the picture.
const NAME: u32 = 1;
const PICTURE: u32 = 2;
/// How far an arrow moves a picture shown at actual size.
const PAN_STEP: i32 = 64;
/// The background around a picture.
const BACKDROP: u32 = 0x00_2c_2c_2e;

struct Shown {
    name: String,
    format: Format,
    image: Image,
    /// The picture at the size it is drawn, and that size.
    scaled: Vec<u32>,
    size: (u32, u32),
}

struct Viewer {
    home: Option<Node>,
    name: String,
    shown: Option<Shown>,
    fit: bool,
    /// Where the window's view starts in a picture at actual size.
    pan: (i32, i32),
    message: String,
    started: bool,
}

impl Viewer {
    fn read(&self, name: &str) -> Result<Vec<u8>, &'static str> {
        let home = self.home.as_ref().ok_or("Files are not available.")?;
        let (file, kind) = home.walk(name, 0).map_err(|_| "There is no such file.")?;
        let result = (|| {
            if kind != Kind::File {
                return Err("That is a folder.");
            }
            let size = file.stat().map_or(0, |stat| stat.size) as usize;
            if size > oceans_image::MAX_FILE {
                return Err("The file is too large (more than 32 MiB).");
            }
            let mut bytes = alloc::vec![0u8; size];
            let mut done = 0;
            while done < size {
                match file.read(done as u64, &mut bytes[done..]) {
                    Ok(0) => break,
                    Ok(got) => done += got,
                    Err(_) => return Err("The file cannot be read."),
                }
            }
            bytes.truncate(done);
            Ok(bytes)
        })();
        file.close();
        result
    }

    fn open(&mut self, name: &str, view: (u32, u32)) {
        let name = String::from(name.trim().trim_matches('/'));
        if name.is_empty() {
            self.message = String::from("Type the name of a picture in Home.");
            return;
        }
        let bytes = match self.read(&name) {
            Ok(bytes) => bytes,
            Err(why) => {
                self.message = String::from(why);
                return;
            }
        };
        match oceans_image::decode(&bytes) {
            Ok((format, image)) => {
                self.name = name.clone();
                self.shown = Some(Shown {
                    name,
                    format,
                    image,
                    scaled: Vec::new(),
                    size: (0, 0),
                });
                self.pan = (0, 0);
                self.message.clear();
                self.prepare(view);
            }
            Err(error) => {
                self.message = alloc::format!("{name}: {}.", error.message());
            }
        }
    }

    /// Makes the picture ready to draw in a `view` sized area: shrunk to
    /// fit, or at actual size.
    fn prepare(&mut self, view: (u32, u32)) {
        let fit = self.fit;
        let Some(shown) = self.shown.as_mut() else {
            return;
        };
        let (w, h) = (shown.image.width, shown.image.height);
        let size = if fit {
            oceans_image::fit(w, h, view.0, view.1)
        } else {
            (w, h)
        };
        if size == shown.size {
            return;
        }
        shown.scaled = if size == (w, h) {
            shown.image.pixels.clone()
        } else {
            oceans_image::scale(&shown.image, size.0, size.1)
        };
        shown.size = size;
    }

    /// The pictures in the shown one's folder, by name.
    fn neighbours(&self) -> (String, Vec<String>) {
        let current = self
            .shown
            .as_ref()
            .map_or(self.name.as_str(), |s| s.name.as_str());
        let folder = current.rsplit_once('/').map_or("", |(folder, _)| folder);
        let mut names = Vec::new();
        let Some(home) = self.home.as_ref() else {
            return (String::from(folder), names);
        };
        let opened = if folder.is_empty() {
            None
        } else {
            match home.walk(folder, 0) {
                Ok((node, Kind::Directory)) => Some(node),
                Ok((node, Kind::File)) => {
                    node.close();
                    return (String::from(folder), names);
                }
                Err(_) => return (String::from(folder), names),
            }
        };
        let directory = opened.as_ref().unwrap_or(home);
        let mut name = [0u8; MAX_NAME];
        for index in 0u32.. {
            match directory.entry(index, &mut name) {
                Ok(Some((Kind::File, len))) => {
                    if let Ok(text) = core::str::from_utf8(&name[..len])
                        && oceans_image::is_image_name(text)
                    {
                        names.push(String::from(text));
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        if let Some(node) = opened {
            node.close();
        }
        names.sort_unstable();
        (String::from(folder), names)
    }

    /// The previous (`step` -1) or next (+1) picture in the folder.
    fn step(&mut self, step: isize, view: (u32, u32)) {
        if self.shown.is_none() {
            self.message = String::from("Open a picture first.");
            return;
        }
        let (folder, names) = self.neighbours();
        let current = self.shown.as_ref().map_or("", |s| s.name.as_str());
        let base = current.rsplit_once('/').map_or(current, |(_, name)| name);
        let Some(at) = names.iter().position(|n| n == base) else {
            return;
        };
        let next = at as isize + step;
        if next < 0 || next as usize >= names.len() {
            self.message = String::from(if step < 0 {
                "This is the first picture in the folder."
            } else {
                "This is the last picture in the folder."
            });
            return;
        }
        let name = if folder.is_empty() {
            names[next as usize].clone()
        } else {
            alloc::format!("{folder}/{}", names[next as usize])
        };
        self.open(&name, view);
    }
}

fn frame(ui: &mut Ui<'_, '_>, app: &mut Viewer) {
    ui.background(colour::WINDOW);
    let whole = ui.area;
    if app.home.is_none() {
        ui.area = Rect::new(whole.x + 20, whole.y + 20, whole.w - 40, whole.h - 40);
        ui.y = ui.area.y;
        ui.heading("Image Viewer");
        ui.muted("Image Viewer may not open your files on this system.");
        return;
    }
    let area = Rect::new(
        whole.x,
        whole.y + TOOLBAR,
        whole.w,
        whole.h - TOOLBAR - STATUS,
    );
    let view = (area.w.max(1) as u32, area.h.max(1) as u32);
    if !app.started {
        app.started = true;
        *ui.focus = Some(PICTURE);
        if !app.name.is_empty() {
            let name = app.name.clone();
            app.open(&name, view);
        }
    }

    // The toolbar: the name, Open, Previous, Next, Fit / Actual size.
    let bar = Rect::new(whole.x, whole.y, whole.w, TOOLBAR);
    ui.surface.fill(bar, colour::SIDEBAR);
    ui.surface
        .fill(Rect::new(bar.x, bar.y + bar.h - 1, bar.w, 1), colour::LINE);
    ui.area = Rect::new(12, 11, 260, 30);
    ui.y = 11;
    if ui.text_field(NAME, &mut app.name, "Picture name, in Home") {
        let name = app.name.clone();
        app.open(&name, view);
        *ui.focus = Some(PICTURE);
    }
    let size_label = if app.fit { "Actual size" } else { "Fit" };
    let buttons = [
        ("Open", 284, 70),
        ("Previous", 362, 88),
        ("Next", 458, 64),
        (size_label, 642, 106),
    ];
    for (text, x, w) in buttons {
        if !ui.button_in(Rect::new(x, 11, w, 30), text, text == "Open") {
            continue;
        }
        match text {
            "Open" => {
                let name = app.name.clone();
                app.open(&name, view);
            }
            "Previous" => app.step(-1, view),
            "Next" => app.step(1, view),
            _ => {
                app.fit = !app.fit;
                app.pan = (0, 0);
                app.prepare(view);
            }
        }
        *ui.focus = Some(PICTURE);
    }

    // A click on the picture gives it the keyboard.
    if let Some((x, y)) = ui.input.click
        && area.contains(x, y)
    {
        ui.input.click = None;
        ui.changed = true;
        *ui.focus = Some(PICTURE);
    }
    if *ui.focus == Some(PICTURE) && ui.input.focused {
        for key in core::mem::take(&mut ui.input.keys) {
            ui.changed = true;
            match key {
                KEY_PAGE_UP => app.step(-1, view),
                KEY_PAGE_DOWN | b' ' => app.step(1, view),
                KEY_LEFT => app.pan.0 -= PAN_STEP,
                KEY_RIGHT => app.pan.0 += PAN_STEP,
                KEY_UP => app.pan.1 -= PAN_STEP,
                KEY_DOWN => app.pan.1 += PAN_STEP,
                b'f' | b'F' => {
                    app.fit = !app.fit;
                    app.pan = (0, 0);
                    app.prepare(view);
                }
                _ => {}
            }
        }
    }

    // The picture, centred, or panned when larger than the view.
    ui.surface.fill(area, oceans_ui::Rgb(BACKDROP));
    let mut status = String::new();
    if let Some(shown) = app.shown.as_ref() {
        let (w, h) = (shown.size.0 as i32, shown.size.1 as i32);
        let limit = |size: i32, room: i32, pan: i32| pan.clamp(0, (size - room).max(0));
        app.pan = (limit(w, area.w, app.pan.0), limit(h, area.h, app.pan.1));
        let x0 = area.x + ((area.w - w) / 2).max(0);
        let y0 = area.y + ((area.h - h) / 2).max(0);
        let surface_width = ui.surface.width;
        let pixels = ui.surface.pixels();
        for row in 0..h.min(area.h) {
            let sy = row + app.pan.1;
            let y = y0 + row;
            for column in 0..w.min(area.w) {
                let sx = column + app.pan.0;
                let x = x0 + column;
                let pixel = shown.scaled[(sy * w + sx) as usize];
                pixels[(y * surface_width + x) as usize] = oceans_image::over_checker(pixel, x, y);
            }
        }
        let percent = shown.size.0 as u64 * 100 / u64::from(shown.image.width.max(1));
        status = alloc::format!(
            "{}  ·  {} × {} {}  ·  {}%",
            shown.name,
            shown.image.width,
            shown.image.height,
            shown.format.name(),
            percent
        );
    } else if app.message.is_empty() {
        let hint = "Open a PNG or BMP picture from Home.";
        let width = ui.measure(hint, Style::Body);
        ui.text_at(
            area.x + (area.w - width) / 2,
            area.y + area.h / 2 - 10,
            hint,
            Style::Body,
            colour::SURFACE,
            area,
        );
    }

    // The status bar.
    let bar = Rect::new(whole.x, whole.y + whole.h - STATUS, whole.w, STATUS);
    ui.surface.fill(bar, colour::WINDOW);
    ui.surface
        .fill(Rect::new(bar.x, bar.y, bar.w, 1), colour::LINE);
    let end = ui.text_at(
        bar.x + 12,
        bar.y + 5,
        &status,
        Style::Body,
        colour::MUTED,
        bar,
    );
    if !app.message.is_empty() {
        let message = app.message.clone();
        let x = if status.is_empty() {
            bar.x + 12
        } else {
            end + 24
        };
        ui.text_at(x, bar.y + 5, &message, Style::Body, colour::MUTED, bar);
    }
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return oceans_ui::EXIT_BAD_START;
    };
    let mut app = Viewer {
        home: directory.find("use", "files").map(Node),
        name: String::from(directory.args().trim()),
        shown: None,
        fit: true,
        pan: (0, 0),
        message: String::new(),
        started: false,
    };
    oceans_ui::run_resizable(
        &directory, "", WIDTH, HEIGHT, MIN_SIZE, None, &mut app, frame,
    )
}
