extern crate std;

use super::*;
use crate::token::{Token, tokenize};
use std::string::String;
use std::vec::Vec;

/// Every character 8 pixels wide, but the signs over and under Thai
/// letters, which take none.
struct Fixed;

impl Measure for Fixed {
    fn width(&mut self, text: &str, _font: Font) -> i32 {
        text.chars()
            .filter(
                |c| !matches!(c, '\u{0e31}' | '\u{0e34}'..='\u{0e3a}' | '\u{0e47}'..='\u{0e4e}'),
            )
            .count() as i32
            * 8
    }

    fn height(&mut self, font: Font) -> i32 {
        match font {
            Font::Heading => 26,
            _ => 20,
        }
    }
}

fn texts(page: &Page) -> Vec<(&str, i32, i32)> {
    page.texts
        .iter()
        .map(|t| (t.text.as_str(), t.x, t.y))
        .collect()
}

#[test]
fn tokens_and_references() {
    let tokens = tokenize(
        "<!DOCTYPE html><P CLASS=x id='a b' hidden>Fish &amp; chips &#3585;&#x0E02;&copy &bogus;\
         <!-- <b>not</b> --><br/>1 < 2</p><script>if (a<b) {}</script>",
    );
    assert_eq!(
        tokens,
        [
            Token::Start {
                name: "p".into(),
                attrs: std::vec![
                    ("class".into(), "x".into()),
                    ("id".into(), "a b".into()),
                    ("hidden".into(), String::new()),
                ],
                self_closing: false,
            },
            Token::Text("Fish & chips กข© &bogus;".into()),
            Token::Start {
                name: "br".into(),
                attrs: Vec::new(),
                self_closing: true,
            },
            Token::Text("1 < 2".into()),
            Token::End { name: "p".into() },
            Token::Start {
                name: "script".into(),
                attrs: Vec::new(),
                self_closing: false,
            },
            Token::Text("if (a<b) {}".into()),
            Token::End {
                name: "script".into()
            },
        ]
    );
    assert_eq!(decode("&#0;&#x110000;&lt"), "\u{fffd}\u{fffd}<");
}

#[test]
fn the_tree_forgives() {
    let doc = parse(
        "<html><head><title> A\n page </title><style>p{}</style></head><body>\
         <p>one<p>two<ul><li>a<li>b</ul><h2>H</h3>after<svg><text>no</text></svg>\
         <a href=x>1<a href=y>2</a></body>",
    );
    assert_eq!(doc.title, "A page");
    let names: Vec<&str> = doc
        .children(Document::ROOT)
        .iter()
        .map(|&id| doc.name(id).unwrap_or("#text"))
        .collect();
    assert_eq!(names, ["p", "p", "ul", "h2", "#text", "a", "a"]);
    let ul = doc.children(Document::ROOT)[2];
    assert_eq!(doc.children(ul).len(), 2, "the second li closed the first");
    let text = |id| match &doc.nodes[id] {
        Node::Text(t) => t.as_str(),
        _ => "",
    };
    assert_eq!(text(doc.children(Document::ROOT)[4]), "after");
}

#[test]
fn deep_and_damaged_pages_do_not_hurt() {
    let deep = "<div>".repeat(10_000) + "deep";
    let page = layout(&parse(&deep), 300, &mut Fixed);
    assert_eq!(texts(&page), [("deep", 0, 0)]);
    // Cut anywhere: no panic.
    let html = "<p class=\"a\">x &amp; <a href='/b'>y</a><!-- c --><pre>\n z</pre>&#x0E01";
    for len in 0..=html.len() {
        if html.is_char_boundary(len) {
            let _ = layout(&parse(&html[..len]), 50, &mut Fixed);
        }
    }
    for junk in [
        "<", "</", "<a", "<!", "<!--", "&", "&#", "&#x", "<a b='", "</>", "<<>>",
    ] {
        let _ = layout(&parse(junk), 1, &mut Fixed);
    }
}

#[test]
fn blocks_lines_and_links() {
    let doc = parse(
        "<h1>Lagoon</h1><p>Hello   <b>bold</b> world, <a href=\"/next\">a link</a>.</p>\
         <ol start=3><li>three<li>four</ol><hr><pre>\n a  b\n\tc</pre>",
    );
    let page = layout(&doc, 400, &mut Fixed);
    assert_eq!(
        texts(&page),
        [
            ("Lagoon", 0, 0),
            ("Hello", 0, 42),
            ("bold", 48, 42),
            ("world,", 88, 42),
            ("a link", 144, 42),
            (".", 192, 42),
            ("3.", 4, 72),
            ("three", 28, 72),
            ("4.", 4, 92),
            ("four", 28, 92),
            (" a  b", 0, 133),
            ("        c", 0, 153),
        ]
    );
    assert_eq!(
        page.rules,
        [Rule {
            x: 0,
            y: 122,
            width: 400
        }]
    );
    assert_eq!(page.height, 173);
    assert_eq!(page.link_at(150, 50), Some("/next"));
    assert_eq!(page.link_at(140, 50), None);
    assert_eq!(
        page.plain_text(),
        "Lagoon\nHello bold world, a link.\n3. three\n4. four\n a  b\n        c"
    );
}

#[test]
fn lines_break_between_words_and_thai_clusters() {
    let page = layout(&parse("<p>one two three four</p>"), 72, &mut Fixed);
    assert_eq!(
        texts(&page),
        [("one two", 0, 0), ("three", 0, 20), ("four", 0, 40)]
    );
    // A word wider than the line is cut where it must be.
    let page = layout(&parse("abcdefghij"), 32, &mut Fixed);
    assert_eq!(
        texts(&page),
        [("abcd", 0, 0), ("efgh", 0, 20), ("ij", 0, 40)]
    );
    // Thai: never before a sign over or after a consonant, never after a
    // leading vowel.
    let thai = "ภาษาไทยเขียนติดกัน";
    let page = layout(&parse(thai), 56, &mut Fixed);
    let lines: Vec<&str> = page.texts.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(lines.concat(), thai, "nothing lost");
    assert!(lines.len() > 1);
    for line in &lines {
        let first = line.chars().next().unwrap();
        let last = line.chars().last().unwrap();
        assert!(!layout::thai_breaks("ก").contains(&0));
        assert!(
            !matches!(first, '\u{0e30}'..='\u{0e3a}' | '\u{0e47}'..='\u{0e4e}'),
            "{line}"
        );
        assert!(!matches!(last, '\u{0e40}'..='\u{0e44}'), "{line}");
    }
}

#[test]
fn plain_text_keeps_its_lines() {
    let page = layout_plain("a b\r\n\r\nc", 400, &mut Fixed);
    assert_eq!(texts(&page), [("a b", 0, 0), ("c", 0, 40)]);
    assert!(page.texts.iter().all(|t| t.font == Font::Mono));
}

#[test]
fn links_resolve_against_the_page() {
    let base = "https://example.org/docs/guide/page.html?x=1#top";
    let cases = [
        (
            "other.html",
            Some("https://example.org/docs/guide/other.html"),
        ),
        ("../up/", Some("https://example.org/docs/up/")),
        ("../../../../far", Some("https://example.org/far")),
        ("./", Some("https://example.org/docs/guide/")),
        ("/root?q", Some("https://example.org/root?q")),
        ("?y=2", Some("https://example.org/docs/guide/page.html?y=2")),
        (
            "#part",
            Some("https://example.org/docs/guide/page.html?x=1#part"),
        ),
        (
            "//cdn.example.net/a/./b",
            Some("https://cdn.example.net/a/b"),
        ),
        ("HTTP://Other.example/", Some("HTTP://Other.example/")),
        (" http://a.example\n/x ", Some("http://a.example/x")),
        ("mailto:someone@example.org", None),
        ("javascript:alert(1)", None),
    ];
    for (href, expected) in cases {
        assert_eq!(url::resolve(base, href).as_deref(), expected, "{href}");
    }
    assert_eq!(
        url::resolve("http://host.example", "a").as_deref(),
        Some("http://host.example/a")
    );
    assert_eq!(
        url::resolve("http://host.example:8080/a/b", "c").as_deref(),
        Some("http://host.example:8080/a/c")
    );
}

#[test]
fn thai_pages_in_tis_620() {
    let tis = [0x3c, 0x70, 0x3e, 0xc0, 0xd2, 0xC9, 0xD2, 0xE4, 0xB7, 0xC2];
    assert_eq!(
        decode_page(&tis, Some("text/html; charset=TIS-620")),
        "<p>ภาษาไทย"
    );
    let mut meta = std::vec::Vec::from(&b"<meta charset=\"windows-874\">"[..]);
    meta.extend_from_slice(&[0xa1, 0x96]);
    assert_eq!(decode_page(&meta, None), "<meta charset=\"windows-874\">ก–");
    assert_eq!(decode_page("ไทย".as_bytes(), Some("text/html")), "ไทย");
    assert_eq!(decode_page(&[0xff], None), "\u{fffd}");
}
