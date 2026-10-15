//! HTML for Lagoon, Oceans' web browser (ADR-0157): a page read as
//! browsers read one, forgivingly ([`parse`]), and laid out for a window's
//! width ([`layout`]): headings, paragraphs, lists, quotes, preformatted
//! text and links, Thai broken between clusters.
//!
//! The page is untrusted: nothing in it can make this panic, loop without
//! end or recurse without bound (the tree is at most [`tree::MAX_DEPTH`]
//! deep). No script runs; styles, scripts and embedded media are left out.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod charset;
mod entity;
pub mod layout;
pub mod token;
pub mod tree;
pub mod url;

pub use charset::decode_page;
pub use entity::decode;
pub use layout::{Font, Measure, Page, Rule, Text, layout, layout_plain};
pub use tree::{Document, Node, NodeId, parse};

#[cfg(test)]
mod tests;
