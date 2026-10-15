//! HTML into tokens (after WHATWG HTML §13.2.5, simplified): start and end
//! tags with their attributes, text, and what is skipped (comments,
//! doctypes). Never fails: anything malformed becomes text or is dropped,
//! as browsers do.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Start {
        /// Lower case.
        name: String,
        /// Names lower case; values with character references decoded.
        attrs: Vec<(String, String)>,
        self_closing: bool,
    },
    End {
        name: String,
    },
    Text(String),
}

/// Elements whose content is raw text, ended only by their own end tag.
const RAW_TEXT: [&str; 7] = [
    "script", "style", "textarea", "title", "xmp", "noembed", "iframe",
];

/// Splits `html` into tokens.
pub fn tokenize(html: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let bytes = html.as_bytes();
    let mut at = 0;
    let mut text_start = 0;
    let flush = |tokens: &mut Vec<Token>, from: usize, to: usize| {
        if to > from {
            let text = crate::entity::decode(&html[from..to]);
            if let Some(Token::Text(previous)) = tokens.last_mut() {
                previous.push_str(&text);
            } else {
                tokens.push(Token::Text(text));
            }
        }
    };
    while at < bytes.len() {
        if bytes[at] != b'<' {
            at += 1;
            continue;
        }
        let rest = &html[at..];
        // A comment, a doctype, a CDATA section or a processing instruction.
        if let Some(comment) = rest.strip_prefix("<!--") {
            flush(&mut tokens, text_start, at);
            at = comment
                .find("-->")
                .map_or(bytes.len(), |end| at + 4 + end + 3);
            text_start = at;
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            flush(&mut tokens, text_start, at);
            at = rest.find('>').map_or(bytes.len(), |end| at + end + 1);
            text_start = at;
            continue;
        }
        let end_tag = rest.starts_with("</");
        let name_at = at + if end_tag { 2 } else { 1 };
        if !bytes.get(name_at).is_some_and(u8::is_ascii_alphabetic) {
            // `<` that starts no tag is text.
            at += 1;
            continue;
        }
        flush(&mut tokens, text_start, at);
        let (token, next) = tag(html, name_at, end_tag);
        at = next;
        text_start = at;
        let raw = match &token {
            Token::Start { name, .. } if RAW_TEXT.contains(&name.as_str()) => Some(name.clone()),
            _ => None,
        };
        tokens.push(token);
        // Raw text: everything up to `</name`, as text.
        if let Some(name) = raw {
            let close = find_close(html, at, &name).unwrap_or(bytes.len());
            if close > at {
                let content = &html[at..close];
                let text = if name == "title" || name == "textarea" {
                    crate::entity::decode(content)
                } else {
                    String::from(content)
                };
                tokens.push(Token::Text(text));
            }
            at = close;
            text_start = at;
        }
    }
    flush(&mut tokens, text_start, bytes.len());
    tokens
}

/// Where `</name` starts at or after `from`, case-insensitively.
fn find_close(html: &str, from: usize, name: &str) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut at = from;
    while let Some(found) = html[at..].find("</") {
        let start = at + found;
        let candidate = bytes.get(start + 2..start + 2 + name.len())?;
        if candidate.eq_ignore_ascii_case(name.as_bytes()) {
            return Some(start);
        }
        at = start + 2;
    }
    None
}

/// The tag whose name starts at `at`; returns it and where it ends.
fn tag(html: &str, at: usize, end_tag: bool) -> (Token, usize) {
    let bytes = html.as_bytes();
    let mut i = at;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'/' && bytes[i] != b'>'
    {
        i += 1;
    }
    let name = html[at..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    let mut self_closing = false;
    loop {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            if bytes[i] == b'/' {
                self_closing = true;
            }
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] == b'>' {
            i += 1;
            break;
        }
        self_closing = false;
        // An attribute's name, then maybe `=` and a value.
        let start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && !matches!(bytes[i], b'=' | b'>' | b'/')
        {
            i += 1;
        }
        let attr = html[start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if bytes.get(i) == Some(&b'=') {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            match bytes.get(i) {
                Some(&quote @ (b'"' | b'\'')) => {
                    let from = i + 1;
                    let to = html[from..]
                        .find(char::from(quote))
                        .map_or(bytes.len(), |end| from + end);
                    value = crate::entity::decode(&html[from..to]);
                    i = (to + 1).min(bytes.len());
                }
                _ => {
                    let from = i;
                    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                        i += 1;
                    }
                    value = crate::entity::decode(&html[from..i]);
                }
            }
        }
        if !attr.is_empty() && !end_tag && !attrs.iter().any(|(n, _): &(String, String)| *n == attr)
        {
            attrs.push((attr, value));
        }
    }
    let token = if end_tag {
        Token::End { name }
    } else {
        Token::Start {
            name,
            attrs,
            self_closing,
        }
    };
    (token, i)
}
