//! `mouse [COUNT [SECONDS]]`: prints pointer events as the input service
//! delivers them, until COUNT events (default 10) or SECONDS without
//! enough (default 30) (needs `use:input`, ADR-0042).

#![no_std]
#![no_main]

use core::fmt::Write;

use oceans_input_proto::{Event, Kind, Subscription};
use oceans_rt::{Out, Start};
use utils::{EXIT_FAILED, EXIT_USAGE, console, require};

oceans_rt::manifest!(b"grant out\ngrant use:input\n");
oceans_rt::entry!(main);

/// Notification bits: events are waiting; the time is up.
const EVENTS: u64 = 1 << 0;
const TIMEOUT: u64 = 1 << 1;
const MAX_COUNT: u32 = 1000;
const MAX_SECONDS: u64 = 3600;

fn main(start: Start) -> i64 {
    let (mut out, directory) = match console(&start) {
        Ok(found) => found,
        Err(code) => return code,
    };
    let input = match require(&mut out, &directory, "mouse", "use", "input", "use:input") {
        Ok(input) => input,
        Err(code) => return code,
    };
    let args = directory.args();
    let mut words = args.split_whitespace();
    let count = match words.next() {
        None => Some(10),
        Some(word) => word.parse().ok().filter(|&n| (1..=MAX_COUNT).contains(&n)),
    };
    let seconds = match words.next() {
        None => Some(30),
        Some(word) => word
            .parse()
            .ok()
            .filter(|&n| (1..=MAX_SECONDS).contains(&n)),
    };
    let (Some(count), Some(seconds), None) = (count, seconds, words.next()) else {
        let _ = writeln!(
            out,
            "usage: mouse [COUNT [SECONDS]]   (COUNT 1-{MAX_COUNT}, SECONDS 1-{MAX_SECONDS})"
        );
        return EXIT_USAGE;
    };
    let Ok(notification) = oceans_rt::notification_create() else {
        let _ = writeln!(out, "mouse: cannot create a notification");
        return EXIT_FAILED;
    };
    let subscription = match Subscription::new(input, notification, EVENTS) {
        Ok(subscription) => subscription,
        Err(error) => {
            let _ = writeln!(out, "mouse: {}", error.message());
            return EXIT_FAILED;
        }
    };
    let _ = oceans_rt::timer_set(notification, TIMEOUT, seconds * 1000);
    let _ = writeln!(out, "mouse: waiting for {count} events");
    let mut seen = 0u32;
    while seen < count {
        let bits = match oceans_rt::notification_wait(notification) {
            Ok(bits) => bits,
            Err(error) => {
                let _ = writeln!(out, "mouse: waiting failed: {error:?}");
                return EXIT_FAILED;
            }
        };
        if bits & EVENTS != 0 {
            loop {
                let batch = match subscription.read() {
                    Ok(batch) => batch,
                    Err(error) => {
                        let _ = writeln!(out, "mouse: {}", error.message());
                        return EXIT_FAILED;
                    }
                };
                if batch.lost > 0 {
                    let _ = writeln!(out, "mouse: {} events lost", batch.lost);
                }
                if batch.is_empty() {
                    break;
                }
                for event in batch.events() {
                    if seen < count {
                        print(&mut out, event);
                        seen += 1;
                    }
                }
            }
        }
        if bits & TIMEOUT != 0 && seen < count {
            let _ = writeln!(out, "mouse: timed out after {seen} events");
            return EXIT_FAILED;
        }
    }
    let _ = oceans_rt::timer_set(notification, TIMEOUT, 0);
    0
}

fn print(out: &mut Out, event: &Event) {
    let _ = write!(out, "{} ms  pointer {}: ", event.time_ms, event.device);
    let _ = match event.kind {
        Kind::Motion { dx, dy } => writeln!(out, "motion dx={dx} dy={dy}"),
        Kind::Absolute { x, y, x_max, y_max } => {
            writeln!(out, "absolute x={x} y={y} (of {x_max} x {y_max})")
        }
        Kind::Button { button, pressed } => {
            let state = if pressed { "down" } else { "up" };
            match oceans_input::button_name(button) {
                Some(name) => writeln!(out, "button {button} ({name}) {state}"),
                None => writeln!(out, "button {button} {state}"),
            }
        }
        Kind::Wheel {
            vertical,
            horizontal,
        } => writeln!(out, "wheel vertical={vertical} horizontal={horizontal}"),
    };
}
