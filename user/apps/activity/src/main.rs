//! Activity Monitor: what runs and how much memory it takes, an app that
//! comes with the system (ADR-0080, ADR-0083), on the app toolkit.
//!
//! Memory (used of total, as a bar), the number of processes and how long
//! the system has been up; then the processes, largest first. It reads
//! again every second.
//!
//! Its rights: `window` and `system-info` (read-only).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use oceans_abi::sysinfo::{self, MemoryInfo, ProcessRecord};
use oceans_rt::{Directory, Handle, Start};
use oceans_ui::{Rect, Rgb, Style, Ui, colour};

oceans_rt::entry!(main);

const WIDTH: u16 = 640;
const HEIGHT: u16 = 520;
const MAX_PROCESSES: usize = 256;

struct Process {
    id: u64,
    memory: u64,
    name: String,
    exited: Option<i64>,
}

struct Activity {
    sysinfo: Option<Handle>,
}

fn mib(bytes: u64) -> String {
    if bytes >= 10 << 20 {
        alloc::format!("{} MiB", bytes >> 20)
    } else {
        alloc::format!("{} KiB", bytes >> 10)
    }
}

fn processes(sysinfo: Handle) -> Vec<Process> {
    let mut buffer = alloc::vec![0u8; MAX_PROCESSES * ProcessRecord::SIZE];
    let Ok(len) = oceans_rt::system_info(sysinfo, sysinfo::PROCESSES, &mut buffer) else {
        return Vec::new();
    };
    let mut list: Vec<Process> = buffer[..len]
        .as_chunks::<{ ProcessRecord::SIZE }>()
        .0
        .iter()
        .filter_map(|record| ProcessRecord::decode(record))
        .map(|p| Process {
            id: p.id,
            memory: p.memory,
            name: String::from(sysinfo::text(&p.name)),
            exited: p.exit(),
        })
        .collect();
    list.sort_by(|a, b| b.memory.cmp(&a.memory).then(a.id.cmp(&b.id)));
    list
}

/// A card at the top: a title and a value.
fn card(ui: &mut Ui<'_, '_>, r: Rect, title: &str, value: &str) {
    ui.surface.round_fill(r, 10, colour::SURFACE);
    ui.text_at(r.x + 14, r.y + 10, title, Style::Body, colour::MUTED, r);
    ui.text_at(r.x + 14, r.y + 32, value, Style::Title, colour::TEXT, r);
}

fn frame(ui: &mut Ui<'_, '_>, activity: &mut Activity) {
    ui.background(colour::WINDOW);
    let whole = ui.area;
    let Some(sysinfo) = activity.sysinfo else {
        ui.area = Rect::new(20, 20, whole.w - 40, whole.h - 40);
        ui.y = 20;
        ui.heading("Activity Monitor");
        ui.muted("System information is not available.");
        return;
    };
    let list = processes(sysinfo);
    let mut record = [0u8; MemoryInfo::SIZE];
    let memory = oceans_rt::system_info(sysinfo, sysinfo::MEMORY, &mut record)
        .ok()
        .and_then(|len| MemoryInfo::decode(&record[..len]));

    // The cards.
    let (gap, top) = (12, 14);
    let width = (whole.w - 2 * 14 - 2 * gap) / 3;
    let cards = |i: i32| Rect::new(14 + i * (width + gap), top, width, 72);
    let mut text = String::new();
    if let Some(memory) = memory {
        let total = memory.total_frames * memory.page_size;
        let used = (memory.total_frames - memory.free_frames) * memory.page_size;
        let _ = write!(text, "{} of {}", mib(used), mib(total));
        card(ui, cards(0), "Memory used", &text);
        // The bar under it.
        let r = cards(0);
        let bar = Rect::new(r.x + 14, r.y + r.h - 12, r.w - 28, 4);
        ui.surface.round_fill(bar, 2, colour::LINE);
        let filled = (bar.w as u64 * used / total.max(1)) as i32;
        let tone = if used * 10 > total * 9 {
            colour::DANGER
        } else {
            colour::ACCENT
        };
        ui.surface
            .round_fill(Rect::new(bar.x, bar.y, filled.max(4), bar.h), 2, tone);
    }
    text.clear();
    let running = list.iter().filter(|p| p.exited.is_none()).count();
    let _ = write!(text, "{running}");
    card(ui, cards(1), "Processes", &text);
    text.clear();
    let seconds = oceans_rt::clock_ms() / 1000;
    let _ = write!(
        text,
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    );
    card(ui, cards(2), "Up for", &text);

    // The table.
    let table = Rect::new(14, top + 72 + 16, whole.w - 28, whole.h - top - 72 - 30);
    ui.surface.round_fill(table, 10, colour::SURFACE);
    let columns = [
        (table.x + 16, "PID"),
        (table.x + 76, "Name"),
        (table.x + table.w - 200, "Memory"),
        (table.x + table.w - 100, "State"),
    ];
    for (x, title) in columns {
        ui.text_at(x, table.y + 10, title, Style::Strong, colour::MUTED, table);
    }
    ui.surface.fill(
        Rect::new(table.x + 12, table.y + 34, table.w - 24, 1),
        colour::LINE,
    );
    let mut y = table.y + 42;
    for (i, process) in list.iter().enumerate() {
        if y + 22 > table.y + table.h - 6 {
            break;
        }
        if i % 2 == 1 {
            ui.surface.tint(
                Rect::new(table.x + 8, y - 3, table.w - 16, 24),
                6,
                Rgb(0),
                8,
            );
        }
        let mut cell = String::new();
        let _ = write!(cell, "{}", process.id);
        ui.text_at(table.x + 16, y, &cell, Style::Body, colour::TEXT, table);
        let name_clip = Rect::new(table.x + 76, table.y, table.w - 290, table.h);
        ui.text_at(
            table.x + 76,
            y,
            &process.name,
            Style::Body,
            colour::TEXT,
            name_clip,
        );
        ui.text_at(
            table.x + table.w - 200,
            y,
            &mib(process.memory),
            Style::Body,
            colour::TEXT,
            table,
        );
        cell.clear();
        match process.exited {
            None => cell.push_str("running"),
            Some(code) => {
                let _ = write!(cell, "exited {code}");
            }
        }
        ui.text_at(
            table.x + table.w - 100,
            y,
            &cell,
            Style::Body,
            colour::MUTED,
            table,
        );
        y += 24;
    }
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return oceans_ui::EXIT_BAD_START;
    };
    let mut activity = Activity {
        sysinfo: directory.find("sysinfo", "sysinfo"),
    };
    oceans_ui::run_ticking(
        &directory,
        "",
        WIDTH,
        HEIGHT,
        Some(1000),
        &mut activity,
        frame,
    )
}
