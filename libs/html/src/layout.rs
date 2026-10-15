//! A document laid out for a width: blocks one under another, their text
//! in lines broken between words (and, in Thai, which has no spaces,
//! between clusters), lists with their markers, links with where they
//! are. The glyphs' widths come from the caller ([`Measure`]); what comes
//! out is a list of pieces of text at positions, for the window to draw.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::tree::{Document, Node, NodeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    Body,
    Strong,
    /// `<h1>` and `<h2>`.
    Heading,
    /// `<pre>`, `<code>` and plain text.
    Mono,
}

/// The fonts' sizes, from whoever draws them.
pub trait Measure {
    fn width(&mut self, text: &str, font: Font) -> i32;
    /// A line's height in `font`.
    fn height(&mut self, font: Font) -> i32;
}

/// Text to draw: its top left, its size, and the link it is part of (an
/// index into [`Page::links`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub text: String,
    pub font: Font,
    pub link: Option<usize>,
}

/// A horizontal line (`<hr>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rule {
    pub x: i32,
    pub y: i32,
    pub width: i32,
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub texts: Vec<Text>,
    pub rules: Vec<Rule>,
    /// Each link's `href`, as written.
    pub links: Vec<String>,
    pub height: i32,
    pub title: String,
}

impl Page {
    /// The link at a point of the page.
    pub fn link_at(&self, x: i32, y: i32) -> Option<&str> {
        self.texts
            .iter()
            .filter(|t| x >= t.x && x < t.x + t.width && y >= t.y && y < t.y + t.height)
            .find_map(|t| t.link)
            .map(|link| self.links[link].as_str())
    }

    /// The page's text, a line of it to a line: what copying it gives.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        let mut last: Option<&Text> = None;
        for text in &self.texts {
            if let Some(last) = last {
                if text.y >= last.y + last.height {
                    out.push('\n');
                } else if text.x > last.x + last.width {
                    out.push(' ');
                }
            }
            out.push_str(&text.text);
            last = Some(text);
        }
        out
    }
}

/// Indent of lists, block quotes and definitions.
pub const INDENT: i32 = 28;
/// Space between a list item's marker and its text.
const MARKER_GAP: i32 = 8;

/// Lays out `doc` for `width` pixels.
pub fn layout(doc: &Document, width: i32, measure: &mut impl Measure) -> Page {
    let mut engine = Engine::new(width, measure);
    engine.page.title = doc.title.clone();
    let style = Inline {
        font: Font::Body,
        link: None,
        pre: false,
    };
    engine.children(doc, Document::ROOT, style);
    engine.finish()
}

/// Lays out plain text: its lines as they are, long ones wrapped.
pub fn layout_plain(text: &str, width: i32, measure: &mut impl Measure) -> Page {
    let mut engine = Engine::new(width, measure);
    engine.preformatted(
        text,
        Inline {
            font: Font::Mono,
            link: None,
            pre: true,
        },
    );
    engine.finish()
}

#[derive(Clone, Copy)]
struct Inline {
    font: Font,
    link: Option<usize>,
    pre: bool,
}

struct Engine<'m, M: Measure> {
    measure: &'m mut M,
    page: Page,
    left: i32,
    right: i32,
    /// The top of the next line.
    y: i32,
    /// The space owed above the next line (margins collapse: the larger
    /// wins).
    margin: i32,
    /// The line being filled; its pieces' `y` are set when it ends.
    line: Vec<Text>,
    x: i32,
    /// Whitespace came since the last word.
    space: bool,
    /// The marker of a list item whose first line has not ended yet.
    marker: Option<String>,
    /// Open lists: the next number, for an ordered one.
    lists: Vec<Option<i64>>,
}

impl<'m, M: Measure> Engine<'m, M> {
    fn new(width: i32, measure: &'m mut M) -> Self {
        Self {
            measure,
            page: Page::default(),
            left: 0,
            right: width.max(1),
            y: 0,
            margin: 0,
            line: Vec::new(),
            x: 0,
            space: false,
            marker: None,
            lists: Vec::new(),
        }
    }

    fn finish(mut self) -> Page {
        self.end_line();
        self.page.height = self.y;
        self.page
    }

    fn children(&mut self, doc: &Document, id: NodeId, style: Inline) {
        for &child in doc.children(id) {
            match &doc.nodes[child] {
                Node::Text(text) if style.pre => self.preformatted(text, style),
                Node::Text(text) => self.flow(text, style),
                Node::Element { name, .. } => self.element(doc, child, name, style),
            }
        }
    }

    fn element(&mut self, doc: &Document, id: NodeId, name: &str, mut style: Inline) {
        match name {
            "br" => return self.line_break(style.font),
            "hr" => {
                self.block_start(8);
                let y = self.y + self.margin;
                self.page.rules.push(Rule {
                    x: self.left,
                    y,
                    width: self.right - self.left,
                });
                self.y = y + 1;
                self.margin = 8;
                return;
            }
            "img" => {
                if let Some(alt) = doc.attr(id, "alt").filter(|a| !a.trim().is_empty()) {
                    self.flow(&format!("[{}]", alt.trim()), style);
                }
                return;
            }
            "b" | "strong" | "th" | "dt" | "h3" | "h4" | "h5" | "h6" => style.font = Font::Strong,
            "h1" | "h2" => style.font = Font::Heading,
            "code" | "kbd" | "samp" | "tt" | "pre" | "xmp" | "listing" => style.font = Font::Mono,
            "a" => {
                if let Some(href) = doc.attr(id, "href") {
                    style.link = Some(self.page.links.len());
                    self.page.links.push(String::from(href.trim()));
                }
            }
            _ => {}
        }
        if matches!(name, "td" | "th") && !self.line.is_empty() {
            // Cells side by side, apart (tables proper are B2's).
            self.x += self.measure.width("    ", Font::Body);
            self.space = false;
        }
        let Some(margin) = block(name) else {
            return self.children(doc, id, style);
        };
        let (left, right) = (self.left, self.right);
        self.block_start(margin);
        match name {
            "ul" | "ol" | "menu" | "dir" => {
                let number = (name == "ol").then(|| {
                    doc.attr(id, "start")
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(1)
                });
                self.lists.push(number);
                self.left += INDENT;
            }
            "blockquote" | "figure" => {
                self.left += INDENT;
                self.right -= INDENT;
            }
            "dd" => self.left += INDENT,
            "li" => {
                let nested = self.lists.len() > 1;
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}.");
                        *number += 1;
                        marker
                    }
                    Some(None) if nested => String::from("◦"),
                    _ => String::from("•"),
                };
                self.marker = Some(marker);
            }
            _ => {}
        }
        // Never narrower than a few letters, however deep the nesting.
        if self.right - self.left < INDENT * 2 {
            self.left = (self.right - INDENT * 2).max(0);
        }
        self.x = self.left;
        if matches!(name, "pre" | "xmp" | "listing") {
            style.pre = true;
            // A newline straight after `<pre>` is not content.
            let mut first = true;
            for &child in doc.children(id) {
                match &doc.nodes[child] {
                    Node::Text(text) => {
                        let text = if first { strip_newline(text) } else { text };
                        self.preformatted(text, style);
                    }
                    Node::Element { name, .. } => self.element(doc, child, name, style),
                }
                first = false;
            }
        } else {
            self.children(doc, id, style);
        }
        self.end_line();
        if matches!(name, "ul" | "ol" | "menu" | "dir") {
            self.lists.pop();
        }
        self.marker = None;
        self.margin = self.margin.max(margin);
        self.left = left;
        self.right = right;
        self.x = left;
    }

    fn block_start(&mut self, margin: i32) {
        self.end_line();
        self.margin = self.margin.max(margin);
    }

    /// Text that flows: whitespace is a space, lines break where they must.
    fn flow(&mut self, text: &str, style: Inline) {
        if text.starts_with(|c: char| c.is_ascii_whitespace()) {
            self.space = true;
        }
        let mut words = text.split_ascii_whitespace().peekable();
        while let Some(word) = words.next() {
            self.word(word, style);
            if words.peek().is_some() {
                self.space = true;
            }
        }
        if text.ends_with(|c: char| c.is_ascii_whitespace()) {
            self.space = true;
        }
    }

    /// Text as written: its spaces kept, its newlines lines.
    fn preformatted(&mut self, text: &str, style: Inline) {
        let mut lines = text.split('\n').peekable();
        while let Some(line) = lines.next() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let line = expand_tabs(line);
            if !line.is_empty() {
                self.space = false;
                self.word(&line, style);
            }
            if lines.peek().is_some() {
                self.line_break(style.font);
            }
        }
    }

    /// Places `word` (no spaces in it, unless preformatted), breaking it
    /// over lines if it must.
    fn word(&mut self, mut word: &str, style: Inline) {
        loop {
            let space = if self.space && !self.line.is_empty() {
                self.measure.width(" ", style.font)
            } else {
                0
            };
            let avail = self.right - self.x - space;
            let width = self.measure.width(word, style.font);
            if width <= avail {
                self.put(word, width, space, style);
                return;
            }
            // Where `word` may break, and the longest part that fits.
            let breaks = if has_thai(word) {
                thai_breaks(word)
            } else {
                Vec::new()
            };
            let fit = self.longest(word, &breaks, avail, style.font);
            match fit {
                Some(at) => {
                    let (head, tail) = word.split_at(at);
                    let head_width = self.measure.width(head, style.font);
                    self.put(head, head_width, space, style);
                    self.end_line();
                    word = tail;
                }
                None if !self.line.is_empty() => self.end_line(),
                None => {
                    // Alone on its line and still too long: broken
                    // anywhere a cluster ends.
                    let anywhere = clusters(word);
                    let at = self
                        .longest(word, &anywhere, avail, style.font)
                        .or_else(|| anywhere.first().copied())
                        .unwrap_or(word.len());
                    let (head, tail) = word.split_at(at);
                    let head_width = self.measure.width(head, style.font);
                    self.put(head, head_width, space, style);
                    self.end_line();
                    if tail.is_empty() {
                        return;
                    }
                    word = tail;
                }
            }
        }
    }

    /// The last of `breaks` before which `word` fits in `avail`.
    fn longest(&mut self, word: &str, breaks: &[usize], avail: i32, font: Font) -> Option<usize> {
        // Widths only grow along the word: a binary search.
        let (mut low, mut high) = (0, breaks.len());
        while low < high {
            let mid = (low + high) / 2;
            if self.measure.width(&word[..breaks[mid]], font) <= avail {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        low.checked_sub(1).map(|i| breaks[i])
    }

    fn put(&mut self, text: &str, width: i32, space: i32, style: Inline) {
        self.space = false;
        if let Some(last) = self.line.last_mut()
            && last.font == style.font
            && last.link == style.link
            && last.x + last.width == self.x
        {
            if space > 0 {
                last.text.push(' ');
            }
            last.text.push_str(text);
            last.width += space + width;
            self.x += space + width;
            return;
        }
        let height = self.measure.height(style.font);
        self.line.push(Text {
            x: self.x + space,
            y: 0,
            width,
            height,
            text: String::from(text),
            font: style.font,
            link: style.link,
        });
        self.x += space + width;
    }

    fn line_break(&mut self, font: Font) {
        if self.line.is_empty() {
            self.y += self.margin + self.measure.height(font);
            self.margin = 0;
        } else {
            self.end_line();
        }
        self.space = false;
    }

    /// Ends the line being filled, if it holds anything.
    fn end_line(&mut self) {
        self.space = false;
        self.x = self.left;
        if self.line.is_empty() {
            return;
        }
        if !self.page.texts.is_empty() || !self.page.rules.is_empty() {
            self.y += self.margin;
        }
        self.margin = 0;
        let height = self.line.iter().map(|t| t.height).max().unwrap_or(0);
        if let Some(marker) = self.marker.take() {
            let font = self.line[0].font;
            let width = self.measure.width(&marker, font);
            let marker_height = self.measure.height(font);
            self.page.texts.push(Text {
                x: (self.left - MARKER_GAP - width).max(0),
                y: self.y + height - marker_height,
                width,
                height: marker_height,
                text: marker,
                font,
                link: None,
            });
        }
        for mut text in self.line.drain(..) {
            // Bottoms aligned: near enough to a common baseline.
            text.y = self.y + height - text.height;
            self.page.texts.push(text);
        }
        self.y += height;
    }
}

/// A block element's margin above and below, or `None` for inline ones.
fn block(name: &str) -> Option<i32> {
    let margin = match name {
        "h1" | "h2" => 16,
        "h3" | "h4" | "h5" | "h6" => 12,
        "p" | "ul" | "ol" | "menu" | "dir" | "dl" | "blockquote" | "pre" | "xmp" | "listing"
        | "table" | "figure" | "fieldset" | "form" | "address" => 10,
        "div" | "li" | "dt" | "dd" | "section" | "article" | "header" | "footer" | "nav"
        | "main" | "aside" | "center" | "details" | "summary" | "figcaption" | "caption" | "tr"
        | "thead" | "tbody" | "tfoot" | "noscript" | "legend" | "hgroup" | "search" => 0,
        _ => return None,
    };
    Some(margin)
}

fn strip_newline(text: &str) -> &str {
    text.strip_prefix("\r\n")
        .or_else(|| text.strip_prefix('\n'))
        .unwrap_or(text)
}

/// Tabs to spaces, to the next multiple of eight.
fn expand_tabs(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut column = 0;
    for c in line.chars() {
        if c == '\t' {
            let to = (column / 8 + 1) * 8;
            while column < to {
                out.push(' ');
                column += 1;
            }
        } else {
            out.push(c);
            column += 1;
        }
    }
    out
}

fn is_thai(c: char) -> bool {
    ('\u{0e01}'..='\u{0e5b}').contains(&c)
}

fn has_thai(text: &str) -> bool {
    text.chars().any(is_thai)
}

/// Signs written over, under or after a consonant: never at a line's
/// start.
fn follows(c: char) -> bool {
    matches!(c, '\u{0e30}'..='\u{0e3a}' | '\u{0e45}' | '\u{0e47}'..='\u{0e4e}')
        || ('\u{0300}'..='\u{036f}').contains(&c)
}

/// Vowels written before their consonant: never at a line's end.
fn leads(c: char) -> bool {
    ('\u{0e40}'..='\u{0e44}').contains(&c)
}

/// Where a word holding Thai may break: between clusters where either side
/// is Thai, never before a following sign or after a leading vowel. With
/// no dictionary this is no syllable's end; it is where a broken word still
/// reads (as `libthai` does without one).
pub fn thai_breaks(word: &str) -> Vec<usize> {
    let mut breaks = Vec::new();
    let mut previous: Option<char> = None;
    for (at, c) in word.char_indices() {
        if let Some(p) = previous
            && (is_thai(p) || is_thai(c))
            && !follows(c)
            && !leads(p)
            && !matches!(c, 'ๆ' | 'ฯ' | '.' | ',' | ')' | ';' | ':' | '!' | '?' | '"')
            && !matches!(p, '(' | '"')
        {
            breaks.push(at);
        }
        previous = Some(c);
    }
    breaks
}

/// Every place but before a following sign: where anything may break.
fn clusters(word: &str) -> Vec<usize> {
    word.char_indices()
        .skip(1)
        .filter(|&(_, c)| !follows(c))
        .map(|(at, _)| at)
        .collect()
}
