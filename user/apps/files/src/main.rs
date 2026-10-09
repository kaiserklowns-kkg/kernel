//! Files: the user's files (`/home`), an app that comes with the system
//! (ADR-0080, ADR-0082), on the app toolkit.
//!
//! - Folders and files, folders first; a click selects (and shows the
//!   start of a text file), a second click opens it: into a folder, or a
//!   file in the app that opens its kind. Open does that too; Open with…
//!   lists the apps that open it, to choose one (ADR-0099).
//! - Back goes up a folder; the path shows where you are.
//! - New folder; Delete, after a confirmation (a folder with what is in
//!   it).
//!
//! Its rights: `window` and `files` (the user's files, nothing else).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_fs_proto::{Kind, MAX_NAME, Node, flags, tree};
use oceans_rt::{Directory, Start};
use oceans_ui::{Rect, Style, Ui, colour};

oceans_rt::entry!(main);

const WIDTH: u16 = 760;
const HEIGHT: u16 = 500;
/// The smallest the window may be resized to (ADR-0097).
const MIN_SIZE: (u16, u16) = (600, 320);
/// Entries listed in one folder.
const MAX_ENTRIES: usize = 300;
/// How much of a file the preview shows.
const PREVIEW: usize = 4096;

struct Entry {
    name: String,
    folder: bool,
    size: u64,
}

struct Files {
    home: Option<Node>,
    /// The folders from `/home` down to the one shown.
    path: Vec<String>,
    entries: Vec<Entry>,
    selected: Option<usize>,
    /// The text of the file selected, or why there is none.
    preview: Option<String>,
    new_folder: String,
    naming: bool,
    confirm_delete: bool,
    message: String,
    stale: bool,
    /// The apps that open the selected file, while the user chooses one
    /// (Open with…, ADR-0099).
    choosing: Option<Vec<(String, String)>>,
}

/// `/home/a/b` as the toolkit's path, from the folders.
fn joined(path: &[String]) -> String {
    path.join("/")
}

fn size_text(size: u64) -> String {
    match size {
        0..1024 => alloc::format!("{size} B"),
        1024..1_048_576 => alloc::format!("{} KB", size.div_ceil(1024)),
        _ => alloc::format!("{:.1} MB", size as f64 / 1_048_576.0),
    }
}

/// The folder shown: Home itself (the handle Core gave, writable), or a
/// folder opened in it, closed after use. (`.` names nothing: Home is not
/// reopened.)
enum Folder<'a> {
    Home(&'a Node),
    Opened(Node),
}

impl core::ops::Deref for Folder<'_> {
    type Target = Node;

    fn deref(&self) -> &Node {
        match self {
            Self::Home(node) => node,
            Self::Opened(node) => node,
        }
    }
}

impl Folder<'_> {
    fn close(self) {
        if let Self::Opened(node) = self {
            node.close();
        }
    }
}

impl Files {
    /// The folder shown (opened writable when `write`).
    fn folder(&self, write: bool) -> Option<Folder<'_>> {
        let home = self.home.as_ref()?;
        if self.path.is_empty() {
            return Some(Folder::Home(home));
        }
        let open = if write { flags::WRITE } else { 0 };
        home.walk(&joined(&self.path), open)
            .ok()
            .filter(|(_, kind)| *kind == Kind::Directory)
            .map(|(node, _)| Folder::Opened(node))
    }

    fn refresh(&mut self) {
        self.stale = false;
        self.selected = None;
        self.preview = None;
        self.confirm_delete = false;
        let mut entries = Vec::new();
        let Some(folder) = self.folder(false) else {
            self.entries.clear();
            self.message = String::from("This folder cannot be opened.");
            return;
        };
        let mut name = [0u8; MAX_NAME];
        for index in 0..MAX_ENTRIES as u32 {
            let Ok(Some((kind, len))) = folder.entry(index, &mut name) else {
                break;
            };
            let name = String::from_utf8_lossy(&name[..len]).into_owned();
            let size = if kind == Kind::File {
                folder
                    .open(&name, 0)
                    .ok()
                    .map(|(file, _)| {
                        let size = file.stat().map_or(0, |stat| stat.size);
                        file.close();
                        size
                    })
                    .unwrap_or(0)
            } else {
                0
            };
            entries.push(Entry {
                folder: kind == Kind::Directory,
                name,
                size,
            });
        }
        folder.close();
        // Folders first, then by name.
        entries.sort_by(|a, b| b.folder.cmp(&a.folder).then_with(|| a.name.cmp(&b.name)));
        self.entries = entries;
    }

    /// Opens entry `index`: into a folder, or a file in the app that opens
    /// its kind (ADR-0099).
    fn open(&mut self, ui: &Ui<'_, '_>, index: usize, app: Option<&str>) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        if entry.folder {
            self.path.push(entry.name.clone());
            self.stale = true;
            return;
        }
        let name = self.home_name(&entry.name);
        self.message = match ui.open_file(app, &name) {
            Ok(()) => alloc::format!("Opening {}…", entry.name),
            Err(why) => String::from(why),
        };
    }

    /// `name` in the folder shown, as Home names it (`folder/name`).
    fn home_name(&self, name: &str) -> String {
        if self.path.is_empty() {
            String::from(name)
        } else {
            alloc::format!("{}/{name}", joined(&self.path))
        }
    }

    /// Shows the start of file `index` (a text file) beside the list.
    fn show(&mut self, index: usize) {
        let Some(entry) = self.entries.get(index).filter(|e| !e.folder) else {
            return;
        };
        let name = entry.name.clone();
        self.preview = Some(match self.read_start(&name) {
            Some(bytes) => match core::str::from_utf8(&bytes) {
                Ok(text) => text.to_string(),
                // Cut in the middle of a character: what comes before it.
                Err(error) if error.error_len().is_none() => {
                    String::from_utf8_lossy(&bytes[..error.valid_up_to()]).into_owned()
                }
                Err(_) => String::from("(not a text file)"),
            },
            None => String::from("(cannot be read)"),
        });
    }

    fn read_start(&self, name: &str) -> Option<Vec<u8>> {
        let folder = self.folder(false)?;
        let opened = folder.open(name, 0);
        folder.close();
        let (file, _) = opened.ok()?;
        let mut bytes = alloc::vec![0u8; PREVIEW];
        let mut done = 0;
        while done < bytes.len() {
            match file.read(done as u64, &mut bytes[done..]) {
                Ok(0) | Err(_) => break,
                Ok(got) => done += got,
            }
        }
        file.close();
        bytes.truncate(done);
        Some(bytes)
    }

    fn create_folder(&mut self) {
        let name = self.new_folder.trim().to_string();
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            self.message = String::from("A folder name has no / and is not . or ..");
            return;
        }
        let Some(folder) = self.folder(true) else {
            self.message = String::from("This folder cannot be changed.");
            return;
        };
        let created = folder
            .open(&name, flags::CREATE_DIRECTORY | flags::WRITE)
            .map(|(node, _)| {
                node.close();
                let _ = folder.sync();
            });
        folder.close();
        self.message = match created {
            Ok(_) => {
                self.new_folder.clear();
                self.naming = false;
                alloc::format!("Created {name}.")
            }
            Err(error) => alloc::format!("{name}: {}", error.message()),
        };
        self.stale = true;
    }

    fn delete(&mut self) {
        let Some(entry) = self.selected.and_then(|i| self.entries.get(i)) else {
            return;
        };
        let (name, is_folder) = (entry.name.clone(), entry.folder);
        let Some(folder) = self.folder(true) else {
            self.message = String::from("This folder cannot be changed.");
            return;
        };
        let removed = if is_folder {
            tree::remove_tree(&folder, &name)
        } else {
            folder.remove(&name)
        };
        let _ = folder.sync();
        folder.close();
        self.message = match removed {
            Ok(()) => alloc::format!("Deleted {name}."),
            Err(error) => alloc::format!("{name}: {}", error.message()),
        };
        self.stale = true;
    }
}

fn frame(ui: &mut Ui<'_, '_>, files: &mut Files) {
    if files.stale {
        files.refresh();
    }
    ui.background(colour::WINDOW);
    let whole = ui.area;
    if files.home.is_none() {
        ui.area = Rect::new(whole.x + 20, whole.y + 20, whole.w - 40, whole.h - 40);
        ui.y = ui.area.y;
        ui.heading("Files");
        ui.muted("Files may not open your files on this system.");
        return;
    }

    // The toolbar: back, where, and the actions.
    let bar = Rect::new(whole.x, whole.y, whole.w, 52);
    ui.surface.fill(bar, colour::SIDEBAR);
    ui.surface
        .fill(Rect::new(bar.x, bar.y + bar.h - 1, bar.w, 1), colour::LINE);
    if !files.path.is_empty() && ui.button_in(Rect::new(12, 11, 64, 30), "Back", false) {
        files.path.pop();
        files.stale = true;
    }
    let mut place = String::from("Home");
    for folder in &files.path {
        let _ = write!(place, " / {folder}");
    }
    ui.text_at(
        88,
        16,
        &place,
        Style::Strong,
        colour::TEXT,
        Rect::new(88, 0, whole.w - 360, 52),
    );
    let new_button = Rect::new(whole.w - 252, 11, 112, 30);
    if ui.button_in(new_button, "New folder", false) {
        files.naming = !files.naming;
        files.confirm_delete = false;
        *ui.focus = Some(1);
    }
    let delete_button = Rect::new(whole.w - 132, 11, 120, 30);
    if files.selected.is_some() && ui.button_in(delete_button, "Delete", false) {
        files.confirm_delete = true;
        files.naming = false;
    }

    // The list on the left, the preview on the right.
    let top = bar.y + bar.h + 12;
    let list_width = 340;
    ui.area = Rect::new(whole.x + 12, top, list_width, whole.h - top - 12);
    ui.y = top;
    if files.naming && ui.text_field(1, &mut files.new_folder, "New folder's name, then Enter") {
        files.create_folder();
    }
    if files.confirm_delete
        && let Some(entry) = files.selected.and_then(|i| files.entries.get(i))
    {
        let question = alloc::format!(
            "Delete {}{}?",
            entry.name,
            if entry.folder { " and all in it" } else { "" }
        );
        ui.label(&question);
        let y = ui.y;
        if ui.button_in(Rect::new(ui.area.x, y, 90, 30), "Delete", true) {
            files.confirm_delete = false;
            files.delete();
        }
        if ui.button_in(Rect::new(ui.area.x + 100, y, 90, 30), "Cancel", false) {
            files.confirm_delete = false;
        }
        ui.y += 38;
    }
    let names: Vec<String> = files
        .entries
        .iter()
        .map(|e| {
            if e.folder {
                alloc::format!("{}/", e.name)
            } else {
                alloc::format!("{}    {}", e.name, size_text(e.size))
            }
        })
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    if let Some(index) = ui.list(&names, files.selected) {
        files.message.clear();
        files.choosing = None;
        if files.selected == Some(index) {
            files.open(ui, index, None);
        } else {
            files.selected = Some(index);
            files.preview = None;
            files.confirm_delete = false;
            files.show(index);
        }
    }
    if files.entries.is_empty() {
        ui.muted("This folder is empty.");
    }
    if !files.message.is_empty() {
        ui.muted(&files.message);
    }

    let side = Rect::new(
        whole.x + list_width + 36,
        top,
        whole.w - list_width - 48,
        whole.h - top - 12,
    );
    ui.surface.round_fill(side, 10, colour::SURFACE);
    let clip = Rect::new(side.x + 14, side.y + 10, side.w - 28, side.h - 20);
    match files
        .selected
        .and_then(|i| files.entries.get(i).map(|e| (i, e)))
    {
        Some((_, entry)) if entry.folder => {
            ui.text_at(
                clip.x,
                clip.y,
                &entry.name,
                Style::Strong,
                colour::TEXT,
                clip,
            );
            let hint = "Click again to open the folder.";
            ui.text_at(clip.x, clip.y + 28, hint, Style::Body, colour::MUTED, clip);
        }
        Some((index, entry)) => {
            let name = entry.name.clone();
            ui.text_at(clip.x, clip.y, &name, Style::Strong, colour::TEXT, clip);
            // Open (in the app for its kind) and Open with… (ADR-0099).
            let mut y = clip.y + 28;
            if ui.button_in(Rect::new(clip.x, y, 80, 30), "Open", true) {
                files.choosing = None;
                files.open(ui, index, None);
            }
            if ui.button_in(Rect::new(clip.x + 90, y, 120, 30), "Open with…", false) {
                files.choosing = match files.choosing {
                    Some(_) => None,
                    None => Some(ui.openers(&files.home_name(&name))),
                };
            }
            y += 40;
            let mut chosen = None;
            if let Some(apps) = &files.choosing {
                if apps.is_empty() {
                    let none = "No app opens this kind of file.";
                    ui.text_at(clip.x, y, none, Style::Body, colour::MUTED, clip);
                    y += 26;
                }
                for (id, app) in apps {
                    if ui.button_in(Rect::new(clip.x, y, 240, 30), app, false) {
                        chosen = Some(id.clone());
                    }
                    y += 36;
                }
                y += 4;
            }
            if let Some(id) = chosen {
                files.choosing = None;
                files.open(ui, index, Some(&id));
            }
            let text = files.preview.as_deref().unwrap_or("");
            for line in text.lines() {
                if y + 18 > clip.y + clip.h {
                    break;
                }
                ui.text_at(clip.x, y, line, Style::Body, colour::TEXT, clip);
                y += 20;
            }
        }
        None => {
            let clip = Rect::new(side.x + 14, side.y + 10, side.w - 28, side.h - 20);
            ui.text_at(
                clip.x,
                clip.y,
                "Choose a file or a folder.",
                Style::Body,
                colour::MUTED,
                clip,
            );
        }
    }
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return oceans_ui::EXIT_BAD_START;
    };
    let mut files = Files {
        home: directory.find("use", "files").map(Node),
        path: Vec::new(),
        entries: Vec::new(),
        selected: None,
        preview: None,
        new_folder: String::new(),
        naming: false,
        confirm_delete: false,
        message: String::new(),
        stale: true,
        choosing: None,
    };
    oceans_ui::run_resizable(
        &directory, "", WIDTH, HEIGHT, MIN_SIZE, None, &mut files, frame,
    )
}
