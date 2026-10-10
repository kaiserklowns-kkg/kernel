//! Matching and ranking names for search (ADR-0108): the desktop's
//! launcher ranks app names with it, and Oceans Core file names in Home.
//!
//! A query matches a name when every one of its words is in the name,
//! case aside (ASCII letters; other text as written, Thai included). How
//! well it matches ([`rank`], lower is better):
//! - [`Rank::Whole`]: the name is the query;
//! - [`Rank::Start`]: the name starts with it;
//! - [`Rank::Word`]: a word of the name starts with each of its words;
//! - [`Rank::Inside`]: each is somewhere inside.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// Longest query, in bytes.
pub const MAX_QUERY: usize = 64;

/// How well a name matches, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    Whole,
    Start,
    Word,
    Inside,
}

fn fold(text: &str) -> String {
    text.chars().map(|c| c.to_ascii_lowercase()).collect()
}

/// Whether `at` in `name` begins a word: the start, or after a space or
/// one of `-_.(`.
fn word_start(name: &str, at: usize) -> bool {
    at == 0
        || name[..at]
            .chars()
            .next_back()
            .is_some_and(|c| matches!(c, ' ' | '-' | '_' | '.' | '('))
}

/// How well `name` matches `query`; `None` if it does not, or the query
/// is empty.
pub fn rank(query: &str, name: &str) -> Option<Rank> {
    let query = fold(query.trim());
    if query.is_empty() {
        return None;
    }
    let name = fold(name);
    if name == query {
        return Some(Rank::Whole);
    }
    if name.starts_with(&query) {
        return Some(Rank::Start);
    }
    let mut rank = Rank::Word;
    for word in query.split_whitespace() {
        let mut found = None;
        for (at, _) in name.match_indices(word) {
            if word_start(&name, at) {
                found = Some(Rank::Word);
                break;
            }
            found = Some(Rank::Inside);
        }
        rank = rank.max(found?);
    }
    Some(rank)
}

/// The best `limit` of `names` for `query`, by index: best rank first,
/// then the shorter name, then the order given.
pub fn best<'a>(query: &str, names: impl IntoIterator<Item = &'a str>, limit: usize) -> Vec<usize> {
    let mut found: Vec<(Rank, usize, usize)> = names
        .into_iter()
        .enumerate()
        .filter_map(|(i, name)| rank(query, name).map(|r| (r, name.len(), i)))
        .collect();
    found.sort_unstable();
    found.into_iter().take(limit).map(|(_, _, i)| i).collect()
}

/// The last part of a path (`docs/notes.txt` → `notes.txt`): what a file
/// is matched by.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_whole_start_word_inside() {
        assert_eq!(rank("files", "Files"), Some(Rank::Whole));
        assert_eq!(rank("calc", "Calculator"), Some(Rank::Start));
        assert_eq!(rank("editor", "Text Editor"), Some(Rank::Word));
        assert_eq!(rank("view", "Image Viewer"), Some(Rank::Word));
        assert_eq!(rank("tor", "Text Editor"), Some(Rank::Inside));
        assert_eq!(rank("png", "picture.png"), Some(Rank::Word));
        assert_eq!(rank("zz", "Calculator"), None);
        assert_eq!(rank("", "Calculator"), None);
        assert_eq!(rank("   ", "Calculator"), None);
    }

    #[test]
    fn every_word_must_be_found() {
        assert_eq!(rank("text ed", "Text Editor"), Some(Rank::Start));
        assert_eq!(rank("ed text", "Text Editor"), Some(Rank::Word));
        assert_eq!(rank("ed xyz", "Text Editor"), None);
        assert_eq!(rank("mon act", "Activity Monitor"), Some(Rank::Word));
        assert_eq!(rank("ivi mon", "Activity Monitor"), Some(Rank::Inside));
    }

    #[test]
    fn thai_matches_as_written() {
        assert_eq!(rank("สวัสดี", "สวัสดี Go"), Some(Rank::Start));
        assert_eq!(rank("go", "สวัสดี Go"), Some(Rank::Word));
    }

    #[test]
    fn best_orders_by_rank_then_length() {
        let names = [
            "Text Editor",
            "Editor",
            "Calculator",
            "Edit notes",
            "Credits",
        ];
        assert_eq!(best("edit", names, 10), [1, 3, 0, 4]);
        assert_eq!(best("edit", names, 2), [1, 3]);
        assert!(best("", names, 10).is_empty());
    }

    #[test]
    fn files_match_by_their_name() {
        assert_eq!(file_name("docs/notes/deep.txt"), "deep.txt");
        assert_eq!(file_name("picture.png"), "picture.png");
    }
}
