//! Text Editor: plain text files in the user's files (`/home`), an app that
//! comes with the system (ADR-0080, ADR-0084), on the app toolkit and
//! `oceans-edit`.
//!
//! - A file's name (in Home; `folder/name.txt` for one in a folder), Open
//!   and Save; New for an empty page. Ctrl+S saves too; a page never named
//!   is saved as `untitled.txt`.
//! - The arrows, Home, End, Page Up and Down move; Delete and Backspace
//!   delete; a click puts the cursor.
//! - Changes not saved are not thrown away by Open or New without a second
//!   click.
//!
//! Its rights: `window` and `files` (the user's files, nothing else).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_edit::Editor;
use oceans_fs_proto::{Kind, Node, flags};
use oceans_rt::{Directory, Start};
use oceans_ui::{Rect, Style, Ui, colour};

oceans_rt::entry!(main);

const WIDTH: u16 = 760;
const HEIGHT: u16 = 520;
/// The largest file it opens, and what a page may grow to.
const LIMIT: usize = 256 * 1024;
const LINE: i32 = 22;
const TOOLBAR: i32 = 52;
const STATUS: i32 = 28;
/// The keyboard's owners: the name field and the page.
const NAME: u32 = 1;
const PAGE: u32 = 2;
/// Ctrl+S.
const SAVE_KEY: u8 = 0x13;
/// The name a page never named is saved under.
const UNTITLED: &str = "untitled.txt";

/// What waits for a second click because it would lose changes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    Open,
    New,
}

struct TextEditor {
    home: Option<Node>,
    editor: Editor,
    name: String,
    /// The first line shown, and how far the page is scrolled sideways.
    top: usize,
    left: i32,
    message: String,
    pending: Option<Pending>,
    /// The page has had the keyboard once (it has it at the start).
    started: bool,
}

impl TextEditor {
    fn read(&self, name: &str) -> Result<String, &'static str> {
        let home = self.home.as_ref().ok_or("Files are not available.")?;
        let (file, kind) = home.walk(name, 0).map_err(|_| "There is no such file.")?;
        if kind != Kind::File {
            file.close();
            return Err("That is a folder.");
        }
        let size = file.stat().map_or(0, |stat| stat.size) as usize;
        if size > LIMIT {
            file.close();
            return Err("The file is too large (more than 256 KiB).");
        }
        let mut bytes = alloc::vec![0u8; size];
        let mut done = 0;
        while done < size {
            match file.read(done as u64, &mut bytes[done..]) {
                Ok(0) => break,
                Ok(got) => done += got,
                Err(_) => {
                    file.close();
                    return Err("The file cannot be read.");
                }
            }
        }
        file.close();
        bytes.truncate(done);
        String::from_utf8(bytes).map_err(|_| "This is not a text file.")
    }

    fn open(&mut self) {
        let name = String::from(self.name.trim());
        if name.is_empty() {
            self.message = String::from("Type the name of a file in Home.");
            return;
        }
        match self.read(&name) {
            Ok(text) => {
                self.editor = Editor::from_text(&text, LIMIT);
                self.message = alloc::format!("Opened {name}.");
                self.top = 0;
                self.left = 0;
            }
            Err(why) => self.message = String::from(why),
        }
    }

    fn save(&mut self) {
        if self.name.trim().is_empty() {
            self.name = String::from(UNTITLED);
        }
        let name = String::from(self.name.trim());
        let Some(home) = self.home.as_ref() else {
            self.message = String::from("Files are not available.");
            return;
        };
        let file = match home.walk(&name, flags::CREATE_FILE | flags::WRITE) {
            Ok((file, Kind::File)) => file,
            Ok((folder, Kind::Directory)) => {
                folder.close();
                self.message = String::from("That is a folder.");
                return;
            }
            Err(error) => {
                self.message = alloc::format!("{name}: {}", error.message());
                return;
            }
        };
        let text = self.editor.text();
        let written = file
            .truncate(0)
            .and_then(|()| file.write_all(0, text.as_bytes()))
            .and_then(|()| file.sync());
        file.close();
        self.message = match written {
            Ok(()) => {
                self.editor.saved();
                alloc::format!("Saved {name}.")
            }
            Err(error) => alloc::format!("{name}: {}", error.message()),
        };
    }

    /// Whether `what` may go ahead: at once without changes, or on the
    /// second click.
    fn may(&mut self, what: Pending) -> bool {
        if !self.editor.edited || self.pending == Some(what) {
            self.pending = None;
            return true;
        }
        self.pending = Some(what);
        self.message = String::from("There are changes not saved: click again to lose them.");
        false
    }
}

fn frame(ui: &mut Ui<'_, '_>, app: &mut TextEditor) {
    if !app.started {
        app.started = true;
        *ui.focus = Some(PAGE);
    }
    ui.background(colour::WINDOW);
    let whole = ui.area;
    if app.home.is_none() {
        ui.area = Rect::new(whole.x + 20, whole.y + 20, whole.w - 40, whole.h - 40);
        ui.y = ui.area.y;
        ui.heading("Text Editor");
        ui.muted("Text Editor may not open your files on this system.");
        return;
    }

    // The toolbar: the name, Open, Save, New.
    let bar = Rect::new(whole.x, whole.y, whole.w, TOOLBAR);
    ui.surface.fill(bar, colour::SIDEBAR);
    ui.surface
        .fill(Rect::new(bar.x, bar.y + bar.h - 1, bar.w, 1), colour::LINE);
    ui.area = Rect::new(12, 11, 320, 30);
    ui.y = 11;
    if ui.text_field(NAME, &mut app.name, "File name, in Home") && app.may(Pending::Open) {
        app.open();
    }
    let buttons = [("Open", 344, 72), ("Save", 424, 72), ("New", 504, 64)];
    for (text, x, w) in buttons {
        if !ui.button_in(Rect::new(x, 11, w, 30), text, text == "Save") {
            continue;
        }
        match text {
            "Open" if app.may(Pending::Open) => app.open(),
            "Save" => app.save(),
            "New" if app.may(Pending::New) => {
                app.editor = Editor::new(LIMIT);
                app.name.clear();
                app.top = 0;
                app.left = 0;
                app.message = String::from("A new page.");
            }
            _ => {}
        }
        *ui.focus = Some(PAGE);
    }

    // The page.
    let page = Rect::new(
        whole.x,
        whole.y + TOOLBAR,
        whole.w,
        whole.h - TOOLBAR - STATUS,
    );
    ui.surface.fill(page, colour::SURFACE);
    let text_x = page.x + 16;
    let text_top = page.y + 10;
    let rows = ((page.h - 20) / LINE).max(1) as usize;
    let view = Rect::new(text_x, page.y, page.w - 32, page.h);

    // A click on the page: the keyboard, and the cursor where it landed.
    if let Some((x, y)) = ui.input.click
        && page.contains(x, y)
    {
        ui.input.click = None;
        ui.changed = true;
        *ui.focus = Some(PAGE);
        app.pending = None;
        let line = app.top + ((y - text_top).max(0) / LINE) as usize;
        let line = line.min(app.editor.lines().len() - 1);
        let target = x - text_x + app.left;
        let text = app.editor.lines()[line].clone();
        let mut column = text.len();
        for (i, c) in text.char_indices() {
            let before = ui.measure(&text[..i], Style::Body);
            let after = ui.measure(&text[..i + c.len_utf8()], Style::Body);
            if target < (before + after) / 2 {
                column = i;
                break;
            }
        }
        app.editor.set_cursor(line, column);
    }

    // Keys, when the page has the keyboard.
    if *ui.focus == Some(PAGE) && ui.input.focused {
        for key in core::mem::take(&mut ui.input.keys) {
            ui.changed = true;
            if key == SAVE_KEY {
                app.save();
            } else if app.editor.key(key, rows.saturating_sub(1)) {
                app.pending = None;
            }
        }
    }

    // Keep the cursor in view.
    let (line, column) = app.editor.cursor();
    if line < app.top {
        app.top = line;
    } else if line >= app.top + rows {
        app.top = line + 1 - rows;
    }
    let cursor_text = String::from(&app.editor.lines()[line][..column]);
    let cursor_x = ui.measure(&cursor_text, Style::Body);
    if cursor_x < app.left {
        app.left = (cursor_x - view.w / 3).max(0);
    } else if cursor_x > app.left + view.w - 8 {
        app.left = cursor_x - view.w * 2 / 3;
    }

    let shown: Vec<String> = app
        .editor
        .lines()
        .iter()
        .skip(app.top)
        .take(rows)
        .cloned()
        .collect();
    for (i, text) in shown.iter().enumerate() {
        let y = text_top + i as i32 * LINE;
        ui.text_at(text_x - app.left, y, text, Style::Body, colour::TEXT, view);
    }
    let page_focused = *ui.focus == Some(PAGE) && ui.input.focused;
    if page_focused {
        let x = text_x - app.left + cursor_x;
        let y = text_top + (line - app.top) as i32 * LINE;
        if view.contains(x, y) {
            ui.surface
                .fill(Rect::new(x, y + 1, 2, LINE - 4), colour::ACCENT);
        }
    }

    // The status bar.
    let status = Rect::new(whole.x, whole.y + whole.h - STATUS, whole.w, STATUS);
    ui.surface.fill(status, colour::WINDOW);
    ui.surface
        .fill(Rect::new(status.x, status.y, status.w, 1), colour::LINE);
    let mut text = String::new();
    let _ = write!(
        text,
        "Line {}, Column {}",
        line + 1,
        app.editor.column_chars() + 1
    );
    if app.editor.edited {
        text.push_str("  ·  Edited");
    }
    let end = ui.text_at(
        status.x + 12,
        status.y + 5,
        &text,
        Style::Body,
        colour::MUTED,
        status,
    );
    if !app.message.is_empty() {
        let message = app.message.clone();
        ui.text_at(
            end + 24,
            status.y + 5,
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
    let mut app = TextEditor {
        home: directory.find("use", "files").map(Node),
        editor: Editor::new(LIMIT),
        name: String::new(),
        top: 0,
        left: 0,
        message: String::new(),
        pending: None,
        started: false,
    };
    oceans_ui::run(&directory, "", WIDTH, HEIGHT, &mut app, frame)
}
