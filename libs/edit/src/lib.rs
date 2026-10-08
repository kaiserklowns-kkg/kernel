//! Editing text (ADR-0084): a buffer of lines and a cursor, moved and
//! changed by the bytes keyboards send (ASCII, and `display::KEY_*` for the
//! arrows, Home, End, Delete and the page keys).
//!
//! The text is UTF-8 (a file may hold Thai); the cursor is a byte offset in
//! its line, always on a character boundary. Up and Down keep the column
//! (in characters) the cursor had before them, as editors do.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use oceans_abi::display::{
    CTRL_A, KEY_DELETE, KEY_DOWN, KEY_END, KEY_HOME, KEY_LEFT, KEY_PAGE_DOWN, KEY_PAGE_UP,
    KEY_RIGHT, KEY_UP, unshifted,
};

/// What Tab inserts.
const TAB: &str = "    ";

pub struct Editor {
    lines: Vec<String>,
    line: usize,
    /// Byte offset in the line, on a character boundary.
    column: usize,
    /// The column (in characters) Up and Down aim for.
    goal: Option<usize>,
    /// Bytes of text, newlines counted.
    len: usize,
    /// The most the text may grow to.
    limit: usize,
    /// Changed since [`Editor::saved`].
    pub edited: bool,
    /// Where the selection started (ADR-0095); it runs from here to the
    /// cursor.
    anchor: Option<(usize, usize)>,
}

impl Editor {
    /// An empty buffer that holds at most `limit` bytes.
    pub fn new(limit: usize) -> Self {
        Self {
            lines: vec![String::new()],
            line: 0,
            column: 0,
            goal: None,
            len: 0,
            limit,
            edited: false,
            anchor: None,
        }
    }

    /// `text` in a buffer (CR LF read as LF), the cursor at its start.
    pub fn from_text(text: &str, limit: usize) -> Self {
        let lines: Vec<String> = text
            .split('\n')
            .map(|line| String::from(line.strip_suffix('\r').unwrap_or(line)))
            .collect();
        let len = lines.iter().map(String::len).sum::<usize>() + lines.len() - 1;
        Self {
            lines,
            len,
            ..Self::new(limit)
        }
    }

    /// The text, lines joined by LF.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Bytes of text.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The cursor: line, and byte offset in it.
    pub fn cursor(&self) -> (usize, usize) {
        (self.line, self.column)
    }

    /// The cursor's column in characters (for showing it).
    pub fn column_chars(&self) -> usize {
        self.lines[self.line][..self.column].chars().count()
    }

    /// Puts the cursor at `line`, byte `column` (both clamped; a column
    /// inside a character goes to its start).
    pub fn set_cursor(&mut self, line: usize, column: usize) {
        self.line = line.min(self.lines.len() - 1);
        let text = &self.lines[self.line];
        let mut column = column.min(text.len());
        while !text.is_char_boundary(column) {
            column -= 1;
        }
        self.column = column;
        self.goal = None;
        self.anchor = None;
    }

    /// As [`Editor::set_cursor`], keeping (or starting) a selection from
    /// where the cursor was: a drag, or a click with Shift.
    pub fn select_to(&mut self, line: usize, column: usize) {
        let anchor = self.anchor.unwrap_or((self.line, self.column));
        self.set_cursor(line, column);
        self.anchor = Some(anchor);
    }

    /// The selection, start before end; `None` when nothing is selected.
    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.anchor?;
        let cursor = (self.line, self.column);
        match anchor.cmp(&cursor) {
            core::cmp::Ordering::Less => Some((anchor, cursor)),
            core::cmp::Ordering::Greater => Some((cursor, anchor)),
            core::cmp::Ordering::Equal => None,
        }
    }

    /// The selected text, lines joined by LF.
    pub fn selected_text(&self) -> Option<String> {
        let ((l1, c1), (l2, c2)) = self.selection()?;
        if l1 == l2 {
            return Some(String::from(&self.lines[l1][c1..c2]));
        }
        let mut text = String::from(&self.lines[l1][c1..]);
        for line in &self.lines[l1 + 1..l2] {
            text.push('\n');
            text.push_str(line);
        }
        text.push('\n');
        text.push_str(&self.lines[l2][..c2]);
        Some(text)
    }

    /// Selects everything (Ctrl+A).
    pub fn select_all(&mut self) {
        self.anchor = Some((0, 0));
        self.line = self.lines.len() - 1;
        self.column = self.lines[self.line].len();
        self.goal = None;
    }

    /// Bytes selected, newlines counted.
    fn selected_len(&self) -> usize {
        self.selected_text().map_or(0, |text| text.len())
    }

    /// Deletes the selection, the cursor left where it began; `false` if
    /// nothing was selected.
    pub fn delete_selection(&mut self) -> bool {
        let Some(((l1, c1), (l2, c2))) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let removed = self.selected_len();
        let tail = self.lines[l2].split_off(c2);
        self.lines[l1].truncate(c1);
        self.lines[l1].push_str(&tail);
        self.lines.drain(l1 + 1..=l2);
        self.len -= removed;
        self.line = l1;
        self.column = c1;
        self.goal = None;
        self.anchor = None;
        self.edited = true;
        true
    }

    /// Cuts the selection: its text, gone from the page (Ctrl+X).
    pub fn cut(&mut self) -> Option<String> {
        let text = self.selected_text()?;
        self.delete_selection();
        Some(text)
    }

    /// Marks the text as saved.
    pub fn saved(&mut self) {
        self.edited = false;
    }

    /// Pasted text in place of the selection: CR LF and lone CRs become
    /// line breaks, other control characters but tabs are left out.
    /// `false` when it would pass the limit, and nothing changes.
    pub fn paste(&mut self, text: &str) -> bool {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text: String = text
            .chars()
            .filter(|&c| !c.is_control() || c == '\n' || c == '\t')
            .collect();
        self.insert(&text)
    }

    /// Inserts `text` at the cursor in place of the selection (a newline
    /// splits the line); `false` when it would pass the limit, and nothing
    /// changes.
    pub fn insert(&mut self, text: &str) -> bool {
        if self.len - self.selected_len() + text.len() > self.limit {
            return false;
        }
        self.delete_selection();
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                self.newline();
            }
            self.lines[self.line].insert_str(self.column, part);
            self.column += part.len();
            self.len += part.len();
        }
        self.goal = None;
        self.edited |= !text.is_empty();
        true
    }

    fn newline(&mut self) {
        let rest = self.lines[self.line].split_off(self.column);
        self.lines.insert(self.line + 1, rest);
        self.line += 1;
        self.column = 0;
        self.len += 1;
    }

    /// Deletes the selection, or the character before the cursor (joining
    /// lines at a line's start).
    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.column == 0 && self.line == 0 {
            return;
        }
        self.left();
        self.delete();
    }

    /// Deletes the selection, or the character after the cursor (joining
    /// lines at a line's end).
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let text = &mut self.lines[self.line];
        if let Some(c) = text[self.column..].chars().next() {
            text.remove(self.column);
            self.len -= c.len_utf8();
        } else if self.line + 1 < self.lines.len() {
            let next = self.lines.remove(self.line + 1);
            self.lines[self.line].push_str(&next);
            self.len -= 1;
        } else {
            return;
        }
        self.goal = None;
        self.edited = true;
    }

    pub fn left(&mut self) {
        let text = &self.lines[self.line];
        if let Some(c) = text[..self.column].chars().next_back() {
            self.column -= c.len_utf8();
        } else if self.line > 0 {
            self.line -= 1;
            self.column = self.lines[self.line].len();
        }
        self.goal = None;
    }

    pub fn right(&mut self) {
        let text = &self.lines[self.line];
        if let Some(c) = text[self.column..].chars().next() {
            self.column += c.len_utf8();
        } else if self.line + 1 < self.lines.len() {
            self.line += 1;
            self.column = 0;
        }
        self.goal = None;
    }

    /// Up (`lines` negative) or down by `lines`, keeping the column.
    pub fn vertical(&mut self, lines: isize) {
        let goal = *self.goal.get_or_insert(self.column_chars());
        let line = self.line.saturating_add_signed(lines);
        self.line = line.min(self.lines.len() - 1);
        let text = &self.lines[self.line];
        self.column = text.char_indices().nth(goal).map_or(text.len(), |(i, _)| i);
    }

    pub fn home(&mut self) {
        self.column = 0;
        self.goal = None;
    }

    pub fn end(&mut self) {
        self.column = self.lines[self.line].len();
        self.goal = None;
    }

    /// Moves by a moving key (`KEY_UP` … `KEY_PAGE_DOWN`, not Delete).
    fn moved(&mut self, key: u8, page: isize) {
        match key {
            KEY_UP => self.vertical(-1),
            KEY_DOWN => self.vertical(1),
            KEY_LEFT => self.left(),
            KEY_RIGHT => self.right(),
            KEY_HOME => self.home(),
            KEY_END => self.end(),
            KEY_PAGE_UP => self.vertical(-page),
            KEY_PAGE_DOWN => self.vertical(page),
            _ => {}
        }
    }

    /// Answers one key byte; `page` is how many lines Page Up and Page
    /// Down move. With Shift (`KEY_SHIFTED`) a moving key selects as it
    /// goes; without, Left and Right end a selection at its start and
    /// end, the others move from the cursor (ADR-0095). Ctrl+A selects
    /// everything. `false` for bytes it does not use (other Ctrl+letters,
    /// copying and pasting, Escape), which the app may take.
    pub fn key(&mut self, key: u8, page: usize) -> bool {
        let page = page.max(1) as isize;
        if let Some(moving) = unshifted(key) {
            let anchor = self.anchor.unwrap_or((self.line, self.column));
            self.moved(moving, page);
            self.anchor = Some(anchor);
            return true;
        }
        if let Some(((l1, c1), (l2, c2))) = self.selection()
            && matches!(key, KEY_LEFT | KEY_RIGHT)
        {
            let (line, column) = if key == KEY_LEFT { (l1, c1) } else { (l2, c2) };
            self.set_cursor(line, column);
            return true;
        }
        if matches!(
            key,
            KEY_UP
                | KEY_DOWN
                | KEY_LEFT
                | KEY_RIGHT
                | KEY_HOME
                | KEY_END
                | KEY_PAGE_UP
                | KEY_PAGE_DOWN
        ) {
            self.anchor = None;
            self.moved(key, page);
            return true;
        }
        match key {
            CTRL_A => self.select_all(),
            b'\r' | b'\n' => {
                self.insert("\n");
            }
            b'\t' => {
                self.insert(TAB);
            }
            0x08 | 0x7f => self.backspace(),
            0x20..=0x7e => {
                let byte = [key];
                // ASCII: always UTF-8.
                self.insert(core::str::from_utf8(&byte).unwrap_or_default());
            }
            KEY_DELETE => self.delete(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(editor: &mut Editor, keys: &[u8]) {
        for &key in keys {
            editor.key(key, 10);
        }
    }

    #[test]
    fn typing_enter_and_backspace() {
        let mut editor = Editor::new(1000);
        typed(&mut editor, b"hello\rworld\x7f\x7fx");
        assert_eq!(editor.text(), "hello\nworx");
        assert_eq!(editor.cursor(), (1, 4));
        assert_eq!(editor.len(), editor.text().len());
        assert!(editor.edited);
        // Backspace at a line's start joins it to the one before.
        typed(&mut editor, &[KEY_HOME, 0x7f]);
        assert_eq!(editor.text(), "helloworx");
        assert_eq!(editor.cursor(), (0, 5));
        assert_eq!(editor.len(), 9);
    }

    #[test]
    fn delete_forward_and_joining() {
        let mut editor = Editor::from_text("ab\ncd", 1000);
        typed(&mut editor, &[KEY_DELETE, KEY_END, KEY_DELETE]);
        assert_eq!(editor.text(), "bcd");
        // Nothing after the end.
        typed(&mut editor, &[KEY_END, KEY_DELETE]);
        assert_eq!(editor.text(), "bcd");
        assert_eq!(editor.len(), 3);
    }

    #[test]
    fn up_and_down_keep_the_column() {
        let mut editor = Editor::from_text("long line\nab\nanother line", 1000);
        editor.set_cursor(0, 7);
        typed(&mut editor, &[KEY_DOWN]);
        assert_eq!(editor.cursor(), (1, 2));
        typed(&mut editor, &[KEY_DOWN]);
        assert_eq!(editor.cursor(), (2, 7));
        typed(&mut editor, &[KEY_UP, KEY_UP, KEY_UP]);
        assert_eq!(editor.cursor(), (0, 7));
        // Moving sideways forgets it.
        typed(&mut editor, &[KEY_LEFT, KEY_DOWN]);
        assert_eq!(editor.cursor(), (1, 2));
        typed(&mut editor, &[KEY_PAGE_DOWN]);
        assert_eq!(editor.cursor(), (2, 6));
        typed(&mut editor, &[KEY_PAGE_UP]);
        assert_eq!(editor.cursor(), (0, 6));
    }

    #[test]
    fn left_and_right_cross_lines() {
        let mut editor = Editor::from_text("a\nb", 1000);
        typed(&mut editor, &[KEY_RIGHT, KEY_RIGHT]);
        assert_eq!(editor.cursor(), (1, 0));
        typed(&mut editor, &[KEY_LEFT]);
        assert_eq!(editor.cursor(), (0, 1));
        typed(&mut editor, &[KEY_LEFT, KEY_LEFT]);
        assert_eq!(editor.cursor(), (0, 0));
    }

    #[test]
    fn thai_moves_by_characters() {
        // Three characters, three bytes each.
        let mut editor = Editor::from_text("ไทย\nab", 1000);
        typed(&mut editor, &[KEY_RIGHT]);
        assert_eq!(editor.cursor(), (0, 3));
        assert_eq!(editor.column_chars(), 1);
        typed(&mut editor, b"x");
        assert_eq!(editor.text(), "ไxทย\nab");
        typed(&mut editor, &[KEY_DELETE]);
        assert_eq!(editor.text(), "ไxย\nab");
        typed(&mut editor, &[0x7f, 0x7f]);
        assert_eq!(editor.text(), "ย\nab");
        assert_eq!(editor.len(), editor.text().len());
        // Inside a character: back to its start.
        editor.set_cursor(0, 2);
        assert_eq!(editor.cursor(), (0, 0));
        editor.set_cursor(0, 1);
        typed(&mut editor, &[KEY_DOWN]);
        assert_eq!(editor.cursor(), (1, 0));
    }

    #[test]
    fn the_limit_holds() {
        let mut editor = Editor::new(4);
        typed(&mut editor, b"abcdef");
        assert_eq!(editor.text(), "abcd");
        typed(&mut editor, b"\r\t");
        assert_eq!(editor.text(), "abcd");
        assert!(!editor.insert("x"));
    }

    #[test]
    fn crlf_and_saving() {
        let mut editor = Editor::from_text("a\r\nb\r\n", 1000);
        assert_eq!(editor.lines(), ["a", "b", ""]);
        assert_eq!(editor.text(), "a\nb\n");
        assert_eq!(editor.len(), 4);
        assert!(!editor.edited);
        typed(&mut editor, b"\t");
        assert_eq!(editor.text(), "    a\nb\n");
        editor.saved();
        assert!(!editor.edited);
    }

    #[test]
    fn other_bytes_are_left_to_the_app() {
        let mut editor = Editor::new(10);
        // Ctrl+S, Escape, the next window, copying and pasting.
        for key in [0x13, 0x1b, 0x1e, 0x03, 0x18, 0x16, 0x89, 0x8a, 0x8b, 0x96] {
            assert!(!editor.key(key, 10));
        }
        assert!(editor.is_empty());
        assert!(!editor.edited);
    }

    const SHIFT: u8 = oceans_abi::display::KEY_SHIFTED;

    #[test]
    fn shift_selects_and_typing_replaces_the_selection() {
        let mut editor = Editor::from_text("text edited in oceans", 1000);
        editor.set_cursor(0, 5);
        typed(&mut editor, &[KEY_END | SHIFT]);
        assert_eq!(editor.selected_text().as_deref(), Some("edited in oceans"));
        assert_eq!(editor.selection(), Some(((0, 5), (0, 21))));
        // Shift+Left takes one back; the anchor stays.
        typed(&mut editor, &[KEY_LEFT | SHIFT]);
        assert_eq!(editor.selected_text().as_deref(), Some("edited in ocean"));
        typed(&mut editor, b"X");
        assert_eq!(editor.text(), "text Xs");
        assert_eq!(editor.selection(), None);
        assert_eq!(editor.len(), 7);
        // Left and Right end a selection at its ends.
        editor.set_cursor(0, 0);
        typed(
            &mut editor,
            &[KEY_RIGHT | SHIFT, KEY_RIGHT | SHIFT, KEY_RIGHT],
        );
        assert_eq!((editor.cursor(), editor.selection()), ((0, 2), None));
        typed(&mut editor, &[KEY_LEFT | SHIFT, KEY_LEFT | SHIFT, KEY_LEFT]);
        assert_eq!(editor.cursor(), (0, 0));
        // Other keys forget it.
        typed(&mut editor, &[KEY_END | SHIFT, KEY_HOME]);
        assert_eq!((editor.cursor(), editor.selection()), ((0, 0), None));
    }

    #[test]
    fn selections_across_lines_cut_and_delete() {
        let mut editor = Editor::from_text("one\ntwo\nthree", 1000);
        editor.set_cursor(0, 1);
        typed(&mut editor, &[KEY_DOWN | SHIFT, KEY_DOWN | SHIFT]);
        assert_eq!(editor.selected_text().as_deref(), Some("ne\ntwo\nt"));
        assert_eq!(editor.cut().as_deref(), Some("ne\ntwo\nt"));
        assert_eq!(editor.text(), "ohree");
        assert_eq!((editor.cursor(), editor.len()), ((0, 1), 5));
        assert_eq!(editor.cut(), None);
        // Backspace and Delete take a selection whole.
        typed(&mut editor, &[KEY_END | SHIFT, 0x7f]);
        assert_eq!(editor.text(), "o");
        typed(&mut editor, &[KEY_HOME | SHIFT, KEY_DELETE]);
        assert_eq!(editor.text(), "");
        // Backwards (anchor after the cursor) is the same selection.
        let mut editor = Editor::from_text("abc\ndef", 1000);
        editor.set_cursor(1, 2);
        editor.select_to(0, 1);
        assert_eq!(editor.selected_text().as_deref(), Some("bc\nde"));
    }

    #[test]
    fn select_all_and_paste() {
        let mut editor = Editor::from_text("ไทย\nab", 1000);
        typed(&mut editor, &[CTRL_A]);
        assert_eq!(editor.selected_text().as_deref(), Some("ไทย\nab"));
        assert!(editor.paste("x\r\ny\rz\t\x07!"));
        assert_eq!(editor.text(), "x\ny\nz\t!");
        assert_eq!(editor.cursor(), (2, 3));
        assert_eq!(editor.len(), editor.text().len());
        assert!(editor.edited);
    }

    #[test]
    fn a_paste_past_the_limit_changes_nothing() {
        let mut editor = Editor::from_text("abcd", 6);
        typed(&mut editor, &[KEY_END, KEY_LEFT | SHIFT, KEY_LEFT | SHIFT]);
        // 4 - 2 + 5 > 6: refused, the selection kept.
        assert!(!editor.paste("12345"));
        assert_eq!(editor.text(), "abcd");
        assert_eq!(editor.selected_text().as_deref(), Some("cd"));
        // 4 - 2 + 4 fits.
        assert!(editor.paste("1234"));
        assert_eq!(editor.text(), "ab1234");
    }
}
