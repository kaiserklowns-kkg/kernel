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
    KEY_DELETE, KEY_DOWN, KEY_END, KEY_HOME, KEY_LEFT, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_RIGHT,
    KEY_UP,
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
    }

    /// Marks the text as saved.
    pub fn saved(&mut self) {
        self.edited = false;
    }

    /// Inserts `text` at the cursor (a newline splits the line); `false`
    /// when it would pass the limit, and nothing is inserted.
    pub fn insert(&mut self, text: &str) -> bool {
        if self.len + text.len() > self.limit {
            return false;
        }
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

    /// Deletes the character before the cursor (joining lines at a line's
    /// start).
    pub fn backspace(&mut self) {
        if self.column == 0 && self.line == 0 {
            return;
        }
        self.left();
        self.delete();
    }

    /// Deletes the character after the cursor (joining lines at a line's
    /// end).
    pub fn delete(&mut self) {
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

    /// Answers one key byte; `page` is how many lines Page Up and Page
    /// Down move. `false` for bytes it does not use (Ctrl+letters, Escape),
    /// which the app may take.
    pub fn key(&mut self, key: u8, page: usize) -> bool {
        let page = page.max(1) as isize;
        match key {
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
            KEY_UP => self.vertical(-1),
            KEY_DOWN => self.vertical(1),
            KEY_LEFT => self.left(),
            KEY_RIGHT => self.right(),
            KEY_HOME => self.home(),
            KEY_END => self.end(),
            KEY_DELETE => self.delete(),
            KEY_PAGE_UP => self.vertical(-page),
            KEY_PAGE_DOWN => self.vertical(page),
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
        // Ctrl+S, Escape, the next window.
        for key in [0x13, 0x1b, 0x1e] {
            assert!(!editor.key(key, 10));
        }
        assert!(editor.is_empty());
        assert!(!editor.edited);
    }
}
