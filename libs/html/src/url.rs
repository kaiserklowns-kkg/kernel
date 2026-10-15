//! A link's address made whole against the page's (after RFC 3986 §5.2):
//! `other.html`, `../up`, `/root`, `?query`, `#part` and `//host/path` as
//! browsers take them. Only `http` and `https` addresses come out: a
//! `mailto:`, `javascript:` or `data:` link goes nowhere in Lagoon.

use alloc::string::String;
use alloc::vec::Vec;

/// `href` resolved against `base` (an `http://` or `https://` address).
pub fn resolve(base: &str, href: &str) -> Option<String> {
    // Browsers drop tabs and newlines anywhere in an address, and spaces
    // around it.
    let href: String = href
        .trim()
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    if let Some(scheme) = scheme(&href) {
        return matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https")
            .then(|| normalise(&href));
    }
    let base_scheme = scheme(base)?;
    let after_scheme = &base[base_scheme.len() + 1..];
    let rest = after_scheme.strip_prefix("//")?;
    let authority_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin = &base[..base_scheme.len() + 3 + authority_len];
    let path_and_more = &rest[authority_len..];
    let without_fragment = &base[..base.find('#').unwrap_or(base.len())];
    if href.starts_with("//") {
        return Some(normalise(&alloc::format!("{base_scheme}:{href}")));
    }
    if href.is_empty() || href.starts_with('#') {
        return Some(alloc::format!("{without_fragment}{href}"));
    }
    let path_end = path_and_more
        .find(['?', '#'])
        .unwrap_or(path_and_more.len());
    let path = &path_and_more[..path_end];
    if href.starts_with('?') {
        return Some(alloc::format!("{origin}{path}{href}"));
    }
    if href.starts_with('/') {
        return Some(normalise(&alloc::format!("{origin}{href}")));
    }
    let directory = &path[..path.rfind('/').map_or(0, |at| at + 1)];
    let directory = if directory.is_empty() { "/" } else { directory };
    Some(normalise(&alloc::format!("{origin}{directory}{href}")))
}

/// The scheme `text` starts with (`http` of `http://`), if any.
fn scheme(text: &str) -> Option<&str> {
    let colon = text.find(':')?;
    let scheme = &text[..colon];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    (first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

/// `.` and `..` taken out of an absolute address's path.
fn normalise(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return String::from(url);
    };
    let rest = &url[scheme_end + 3..];
    let authority_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let head = &url[..scheme_end + 3 + authority_len];
    let after = &rest[authority_len..];
    let path_end = after.find(['?', '#']).unwrap_or(after.len());
    let (path, tail) = after.split_at(path_end);
    if path.is_empty() {
        return alloc::format!("{head}/{tail}");
    }
    let mut segments: Vec<&str> = Vec::new();
    let parts: Vec<&str> = path[1..].split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        let last = i + 1 == parts.len();
        match *part {
            "." => {
                if last {
                    segments.push("");
                }
            }
            ".." => {
                segments.pop();
                if last {
                    segments.push("");
                }
            }
            other => segments.push(other),
        }
    }
    let mut out = String::from(head);
    for segment in &segments {
        out.push('/');
        out.push_str(segment);
    }
    if segments.is_empty() {
        out.push('/');
    }
    out.push_str(tail);
    out
}
