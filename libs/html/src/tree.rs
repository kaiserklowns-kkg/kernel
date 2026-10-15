//! Tokens into a document tree, forgiving as browsers are (after WHATWG
//! HTML §13.2.6, much simplified): void elements never open, a paragraph
//! closes when a block starts, list items and table cells close their
//! siblings, a stray end tag is dropped, and what Lagoon does not show
//! (scripts, styles, forms' choices, embedded media) is left out whole.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::token::Token;

/// Elements opened beneath this depth go into the deepest one instead: a
/// page cannot make the layout recurse without end.
pub const MAX_DEPTH: usize = 128;

pub type NodeId = usize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Element {
        name: String,
        attrs: Vec<(String, String)>,
        children: Vec<NodeId>,
    },
    Text(String),
}

/// A parsed page: its nodes, the root (node 0, an element without a name)
/// first.
#[derive(Clone, Debug, Default)]
pub struct Document {
    pub nodes: Vec<Node>,
    /// The `<title>`, whitespace collapsed.
    pub title: String,
}

impl Document {
    pub const ROOT: NodeId = 0;

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        match &self.nodes[id] {
            Node::Element { children, .. } => children,
            Node::Text(_) => &[],
        }
    }

    /// The element's name, or `None` for text.
    pub fn name(&self, id: NodeId) -> Option<&str> {
        match &self.nodes[id] {
            Node::Element { name, .. } => Some(name),
            Node::Text(_) => None,
        }
    }

    pub fn attr(&self, id: NodeId, attr: &str) -> Option<&str> {
        match &self.nodes[id] {
            Node::Element { attrs, .. } => attrs
                .iter()
                .find(|(n, _)| n == attr)
                .map(|(_, v)| v.as_str()),
            Node::Text(_) => None,
        }
    }
}

/// Elements that never have content.
const VOID: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements left out with all they hold.
const SKIPPED: [&str; 13] = [
    "script", "style", "template", "svg", "math", "select", "object", "iframe", "canvas", "audio",
    "video", "textarea", "noembed",
];

/// Elements that close an open paragraph when they start.
const CLOSES_P: [&str; 34] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "ul",
    "li",
    "dd",
    "dt",
];

/// Elements past which a close does not look (§13.2.4.2's scopes, merged):
/// a `</p>` in a table cell does not close a paragraph around the table.
const SCOPE: [&str; 6] = ["table", "td", "th", "caption", "html", "marquee"];

fn heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

/// Parses `html` into a document.
pub fn parse(html: &str) -> Document {
    Builder::new().build(crate::token::tokenize(html))
}

struct Builder {
    doc: Document,
    /// The open elements, the root first.
    open: Vec<NodeId>,
    /// Inside a skipped element: its name and how deeply it nests in itself.
    skipping: Option<(String, usize)>,
    in_title: bool,
}

impl Builder {
    fn new() -> Self {
        let root = Node::Element {
            name: String::new(),
            attrs: Vec::new(),
            children: Vec::new(),
        };
        Self {
            doc: Document {
                nodes: vec![root],
                title: String::new(),
            },
            open: vec![Document::ROOT],
            skipping: None,
            in_title: false,
        }
    }

    fn build(mut self, tokens: Vec<Token>) -> Document {
        for token in tokens {
            if let Some((skipped, depth)) = &mut self.skipping {
                match &token {
                    Token::Start {
                        name,
                        self_closing: false,
                        ..
                    } if name == skipped => *depth += 1,
                    Token::End { name } if name == skipped => {
                        if *depth == 0 {
                            self.skipping = None;
                        } else {
                            *depth -= 1;
                        }
                    }
                    _ => {}
                }
                continue;
            }
            match token {
                Token::Text(text) => self.text(text),
                Token::Start {
                    name,
                    attrs,
                    self_closing,
                } => self.start(name, attrs, self_closing),
                Token::End { name } => self.end(&name),
            }
        }
        let title = core::mem::take(&mut self.doc.title);
        self.doc.title = collapse(&title);
        self.doc
    }

    fn current(&self) -> NodeId {
        *self.open.last().unwrap_or(&Document::ROOT)
    }

    fn current_name(&self) -> &str {
        self.doc.name(self.current()).unwrap_or("")
    }

    fn append(&mut self, node: Node) -> NodeId {
        let id = self.doc.nodes.len();
        self.doc.nodes.push(node);
        let parent = self.current();
        if let Node::Element { children, .. } = &mut self.doc.nodes[parent] {
            children.push(id);
        }
        id
    }

    fn text(&mut self, text: String) {
        if self.in_title {
            self.doc.title.push_str(&text);
            return;
        }
        let parent = self.current();
        // Text after text (a dropped tag between them) joins it.
        if let Some(&last) = self.doc.children(parent).last()
            && let Node::Text(previous) = &mut self.doc.nodes[last]
        {
            previous.push_str(&text);
            return;
        }
        self.append(Node::Text(text));
    }

    fn start(&mut self, name: String, attrs: Vec<(String, String)>, self_closing: bool) {
        match name.as_str() {
            // The document's frame: their content belongs to the root.
            "html" | "head" | "body" | "meta" | "link" | "base" => return,
            "title" => {
                self.in_title = !self_closing;
                return;
            }
            _ => {}
        }
        if SKIPPED.contains(&name.as_str()) {
            // `/>` ends only foreign elements; HTML ignores it (`<div/>`
            // opens a div, `<script/>` a script).
            if !(self_closing && matches!(name.as_str(), "svg" | "math")) {
                self.skipping = Some((name, 0));
            }
            return;
        }
        if CLOSES_P.contains(&name.as_str()) {
            self.close_in_scope("p");
        }
        match name.as_str() {
            "li" => self.close_sibling(&["li"], &["ul", "ol", "menu", "dir"]),
            "dt" | "dd" => self.close_sibling(&["dt", "dd"], &["dl"]),
            "tr" => self.close_sibling(&["tr", "td", "th"], &["table", "thead", "tbody", "tfoot"]),
            "td" | "th" => self.close_sibling(&["td", "th"], &["tr", "table"]),
            "thead" | "tbody" | "tfoot" => {
                self.close_sibling(&["thead", "tbody", "tfoot", "tr", "td", "th"], &["table"])
            }
            "option" => self.close_sibling(&["option"], &[]),
            _ if heading(&name) && heading(self.current_name()) => {
                self.open.pop();
            }
            // `<a>` inside `<a>` closes the first.
            "a" => self.close_in_scope("a"),
            _ => {}
        }
        let void = VOID.contains(&name.as_str());
        let id = self.append(Node::Element {
            name,
            attrs,
            children: Vec::new(),
        });
        if !void && self.open.len() < MAX_DEPTH {
            self.open.push(id);
        }
    }

    fn end(&mut self, name: &str) {
        match name {
            "html" | "head" | "body" => return,
            "title" => {
                self.in_title = false;
                return;
            }
            // `</br>` is a `<br>`, as browsers take it.
            "br" => {
                self.start(String::from("br"), Vec::new(), false);
                return;
            }
            _ => {}
        }
        if heading(name) {
            // `<h2>...</h3>` closes the heading.
            if let Some(at) = self.find_in_scope(heading) {
                self.open.truncate(at);
            }
            return;
        }
        if let Some(at) = self.find_in_scope(|n| n == name) {
            self.open.truncate(at);
        }
    }

    /// Where the nearest open element matching `wanted` is in the stack,
    /// looking no further than a scope boundary.
    fn find_in_scope(&self, wanted: impl Fn(&str) -> bool) -> Option<usize> {
        for (at, &id) in self.open.iter().enumerate().skip(1).rev() {
            let name = self.doc.name(id).unwrap_or("");
            if wanted(name) {
                return Some(at);
            }
            if SCOPE.contains(&name) {
                return None;
            }
        }
        None
    }

    fn close_in_scope(&mut self, name: &str) {
        if let Some(at) = self.find_in_scope(|n| n == name) {
            self.open.truncate(at);
        }
    }

    /// Closes an open `siblings` element, looking no further than one of
    /// `parents`.
    fn close_sibling(&mut self, siblings: &[&str], parents: &[&str]) {
        for at in (1..self.open.len()).rev() {
            let name = self.doc.name(self.open[at]).unwrap_or("");
            if siblings.contains(&name) {
                self.open.truncate(at);
                return;
            }
            if parents.contains(&name) || SCOPE.contains(&name) {
                return;
            }
        }
    }
}

/// `text` with runs of whitespace made one space, trimmed.
pub fn collapse(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for word in text.split_ascii_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}
