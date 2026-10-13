//! Line editing for Oceans' shell (ADR-0133), host-tested and without
//! allocation:
//!
//! - [`History`]: the lines entered, newest last; Up and Down walk them,
//!   and coming back down past the newest gives back what was being typed.
//! - [`Matches`]: Tab completion: the names that start with what was typed,
//!   offered one by one, and what they have in common.
//! - [`last_word`]: where the word being completed starts.

#![cfg_attr(not(test), no_std)]

/// The longest line.
pub const MAX: usize = 200;

/// Lines entered, the oldest dropped when full.
pub struct History<const N: usize> {
    lines: [[u8; MAX]; N],
    lens: [usize; N],
    /// How many are kept, and where the next goes.
    count: usize,
    next: usize,
    /// While walking: how far back (1: the newest), and what was being
    /// typed before.
    back: usize,
    draft: [u8; MAX],
    draft_len: usize,
}

impl<const N: usize> Default for History<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> History<N> {
    pub const fn new() -> Self {
        Self {
            lines: [[0; MAX]; N],
            lens: [0; N],
            count: 0,
            next: 0,
            back: 0,
            draft: [0; MAX],
            draft_len: 0,
        }
    }

    /// A line entered: kept (not an empty one, nor the newest again), and
    /// the walk starts over.
    pub fn push(&mut self, line: &[u8]) {
        self.back = 0;
        let line = &line[..line.len().min(MAX)];
        if line.iter().all(u8::is_ascii_whitespace) || self.newest() == Some(line) {
            return;
        }
        self.lines[self.next][..line.len()].copy_from_slice(line);
        self.lens[self.next] = line.len();
        self.next = (self.next + 1) % N;
        self.count = (self.count + 1).min(N);
    }

    fn get(&self, back: usize) -> &[u8] {
        let at = (self.next + N - back) % N;
        &self.lines[at][..self.lens[at]]
    }

    fn newest(&self) -> Option<&[u8]> {
        (self.count > 0).then(|| self.get(1))
    }

    /// Up: the line before the one shown (`current` is what is typed now,
    /// kept for coming back). `None` at the oldest.
    pub fn older(&mut self, current: &[u8]) -> Option<&[u8]> {
        if self.back >= self.count {
            return None;
        }
        if self.back == 0 {
            let current = &current[..current.len().min(MAX)];
            self.draft[..current.len()].copy_from_slice(current);
            self.draft_len = current.len();
        }
        self.back += 1;
        Some(self.get(self.back))
    }

    /// Down: the line after the one shown, or past the newest what was
    /// being typed. `None` when not walking.
    pub fn newer(&mut self) -> Option<&[u8]> {
        match self.back {
            0 => None,
            1 => {
                self.back = 0;
                Some(&self.draft[..self.draft_len])
            }
            back => {
                self.back = back - 1;
                Some(self.get(self.back))
            }
        }
    }
}

/// Where the last word of `line` starts (after the last space).
pub fn last_word(line: &[u8]) -> usize {
    line.iter().rposition(|&b| b == b' ').map_or(0, |at| at + 1)
}

/// The names that start with a prefix, offered one at a time.
pub struct Matches {
    prefix: [u8; MAX],
    prefix_len: usize,
    /// The longest start all matches share (at least the prefix).
    common: [u8; MAX],
    common_len: usize,
    count: usize,
    /// The one match is a directory (completed with `/`, not a space).
    directory: bool,
}

impl Matches {
    pub fn new(prefix: &[u8]) -> Self {
        let prefix = &prefix[..prefix.len().min(MAX)];
        let mut matches = Self {
            prefix: [0; MAX],
            prefix_len: prefix.len(),
            common: [0; MAX],
            common_len: 0,
            count: 0,
            directory: false,
        };
        matches.prefix[..prefix.len()].copy_from_slice(prefix);
        matches
    }

    /// Whether `name` starts with the prefix.
    pub fn fits(&self, name: &[u8]) -> bool {
        name.starts_with(&self.prefix[..self.prefix_len])
    }

    /// A candidate: counted if it fits (the same name twice counts once).
    pub fn offer(&mut self, name: &[u8], directory: bool) {
        if !self.fits(name) || name.len() > MAX {
            return;
        }
        if self.count == 0 {
            self.common[..name.len()].copy_from_slice(name);
            self.common_len = name.len();
            self.directory = directory;
            self.count = 1;
            return;
        }
        if self.count == 1 && name == &self.common[..self.common_len] {
            return;
        }
        let same = self.common[..self.common_len]
            .iter()
            .zip(name)
            .take_while(|(a, b)| a == b)
            .count();
        self.common_len = same;
        self.count += 1;
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// What to add to what was typed: the rest of the one match and a space
    /// (`/` for a directory), or what all the matches share. Empty: nothing
    /// to add (no match, or several that part right after the prefix).
    pub fn completion(&self, out: &mut [u8; MAX + 1]) -> usize {
        if self.count == 0 {
            return 0;
        }
        let rest = &self.common[self.prefix_len..self.common_len];
        out[..rest.len()].copy_from_slice(rest);
        let mut len = rest.len();
        if self.count == 1 {
            out[len] = if self.directory { b'/' } else { b' ' };
            len += 1;
        }
        len
    }
}

#[cfg(test)]
mod tests;
