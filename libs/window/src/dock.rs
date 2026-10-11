//! What the dock shows (ADR-0109): the apps kept in it, then the windows
//! of the others. A kept app's windows are behind its own icon.

use alloc::string::String;
use alloc::vec::Vec;

/// Apps kept in the dock at most.
pub const MAX_PINNED: usize = 8;

/// An icon of the dock after the apps button and the Terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// A kept app, by its index among the kept apps shown.
    Pinned(usize),
    /// A window of an app not kept.
    Window(u32),
}

/// The dock's icons: the kept apps (`pinned`, by name, in their order),
/// then the windows (`windows`, id and app name) whose app is not kept,
/// in the order they were opened.
pub fn entries(pinned: &[&str], windows: &[(u32, &str)]) -> Vec<Entry> {
    let mut shown: Vec<(u32, &str)> = windows
        .iter()
        .copied()
        .filter(|(_, app)| !pinned.contains(app))
        .collect();
    shown.sort_unstable_by_key(|&(id, _)| id);
    (0..pinned.len())
        .map(Entry::Pinned)
        .chain(shown.into_iter().map(|(id, _)| Entry::Window(id)))
        .collect()
}

/// The first window (opened first) of app `name`, if one is open: a kept
/// app's icon brings it forward.
pub fn first_window(name: &str, windows: &[(u32, &str)]) -> Option<u32> {
    windows
        .iter()
        .filter(|(_, app)| *app == name)
        .map(|&(id, _)| id)
        .min()
}

/// Keeps app `id` in the dock, or takes it out if it was kept: `Some(true)`
/// kept, `Some(false)` taken out, `None` if the dock is full.
pub fn toggle(pinned: &mut Vec<String>, id: &str) -> Option<bool> {
    if let Some(at) = pinned.iter().position(|p| p == id) {
        pinned.remove(at);
        return Some(false);
    }
    if pinned.len() >= MAX_PINNED {
        return None;
    }
    pinned.push(String::from(id));
    Some(true)
}

/// The kept apps as Core stores and sends them: ids, one per line.
pub fn encode(pinned: &[String]) -> String {
    pinned.join("\n")
}

/// The kept apps from [`encode`]'s text: empty lines and repeats dropped,
/// at most [`MAX_PINNED`].
pub fn decode(text: &str) -> Vec<String> {
    let mut pinned: Vec<String> = Vec::new();
    for id in text.lines().map(str::trim).filter(|id| !id.is_empty()) {
        if pinned.len() < MAX_PINNED && !pinned.iter().any(|p| p == id) {
            pinned.push(String::from(id));
        }
    }
    pinned
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use alloc::vec;

    #[test]
    fn kept_apps_come_first_and_take_their_windows() {
        let windows = [(7, "Files"), (3, "Text Editor"), (5, "Files"), (9, "Music")];
        assert_eq!(
            entries(&["Files", "Calculator"], &windows),
            [
                Entry::Pinned(0),
                Entry::Pinned(1),
                Entry::Window(3),
                Entry::Window(9)
            ]
        );
        assert_eq!(
            entries(&[], &windows),
            [
                Entry::Window(3),
                Entry::Window(5),
                Entry::Window(7),
                Entry::Window(9)
            ]
        );
        assert_eq!(first_window("Files", &windows), Some(5));
        assert_eq!(first_window("Calculator", &windows), None);
    }

    #[test]
    fn toggling_keeps_and_takes_out_up_to_the_limit() {
        let mut pinned = vec![String::from("app.a")];
        assert_eq!(toggle(&mut pinned, "app.b"), Some(true));
        assert_eq!(toggle(&mut pinned, "app.a"), Some(false));
        assert_eq!(pinned, ["app.b"]);
        for i in 0..MAX_PINNED - 1 {
            assert_eq!(toggle(&mut pinned, &alloc::format!("app.{i}")), Some(true));
        }
        assert_eq!(toggle(&mut pinned, "app.full"), None);
        assert_eq!(pinned.len(), MAX_PINNED);
    }

    #[test]
    fn the_stored_text_round_trips_and_is_bounded() {
        let pinned = vec![String::from("app.oceans.files"), String::from("app.x")];
        assert_eq!(decode(&encode(&pinned)), pinned);
        assert_eq!(decode("a\n\n a \nb\na\n"), ["a", "b"]);
        let many: String = (0..20).map(|i| alloc::format!("app.{i}\n")).collect();
        assert_eq!(decode(&many).len(), MAX_PINNED);
        assert!(decode("").is_empty());
    }
}
