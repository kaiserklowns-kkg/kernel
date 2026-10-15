//! Lagoon: Oceans' web browser, an app that comes with the system
//! (ADR-0157), on the app toolkit and `oceans-html`. Stage B1: the web
//! read.
//!
//! - An address typed (`https://` is assumed when none is given) and Go,
//!   or a link clicked: the page is fetched (`oceans-web`: redirects
//!   followed, `https` checked against Mozilla's roots), parsed and laid
//!   out for the window's width; again when the width changes.
//! - HTML (headings, paragraphs, lists, quotes, preformatted text, links)
//!   and plain text, in UTF-8 or the Thai TIS-620 / Windows-874. No
//!   script runs; styles, pictures and forms are later stages' (B2, B3).
//! - Back, Forward and Reload; the wheel, the arrows, Page Up and Page
//!   Down, Home and End scroll; Ctrl+C copies the page's text (or the
//!   address field's, when it has the keyboard); a link pointed at shows
//!   its address at the bottom.
//! - Nothing is fetched until asked: it opens on its own start page, or
//!   on the address it was started with (`app start app.oceans.lagoon
//!   ADDRESS`). Where it goes is the user's: it is never written to a log.
//!
//! A page is read a piece at a time between frames, never waited on: the
//! window answers while it comes (only the connection and the response's
//! head are waited for). At most [`MAX_PAGE`] of it is kept.
//!
//! Its rights: `window` and `network`; no files.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use oceans_abi::display::{
    KEY_DOWN, KEY_END, KEY_HOME, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_UP, is_copy,
};
use oceans_html::{Font, Measure, Page};
use oceans_rt::{Directory, Handle, Start};
use oceans_ui::layout::Bar;
use oceans_ui::{Rect, Scroll, Style, Ui, colour};
use oceans_web::Response;

oceans_rt::entry!(main);

const WIDTH: u16 = 900;
const HEIGHT: u16 = 620;
/// The smallest the window may be resized to (ADR-0097).
const MIN_SIZE: (u16, u16) = (480, 320);
/// How often a page that is coming is read.
const TICK_MS: u64 = 100;
const TOOLBAR: i32 = 52;
const STATUS: i32 = 26;
/// Space around the page's text.
const MARGIN: i32 = 20;
/// The widest the page's text is set: lines longer than this are hard to
/// read (as Safari's Reader and Firefox's Reader View keep them).
const MEASURE_MAX: i32 = 760;
/// The most of a page kept.
const MAX_PAGE: usize = 2 * 1024 * 1024;
/// The most read in one tick, so a fast connection does not hold the
/// window.
const TICK_READ: usize = 256 * 1024;
/// How far an arrow scrolls; Page Up and Page Down scroll a view less this.
const LINE: i32 = 44;
/// The keyboard's owners: the address field and the page.
const ADDRESS: u32 = 1;
const PAGE: u32 = 2;
/// The start page's address.
const START: &str = "lagoon:start";
/// The start page: what Lagoon is, without fetching anything.
const START_PAGE: &str = include_str!("start.html");

/// What is shown.
enum Content {
    Html(oceans_html::Document),
    Plain(String),
}

struct Shown {
    url: String,
    content: Content,
    /// Laid out, and for which width.
    page: Page,
    width: i32,
}

/// A page coming.
struct Loading {
    url: String,
    response: Box<Response>,
    body: Vec<u8>,
    content_type: Option<String>,
    /// The status line, when not 2xx: shown under the page.
    status: Option<String>,
}

struct Lagoon {
    net: Option<Handle>,
    address: String,
    shown: Option<Shown>,
    loading: Option<Loading>,
    back: Vec<String>,
    forward: Vec<String>,
    scroll: Scroll,
    message: String,
    /// The address asked for at start, opened on the first frame.
    requested: Option<String>,
    started: bool,
}

/// The toolkit's fonts, measured for the layout.
struct Fonts<'u, 's, 'f>(&'u mut Ui<'s, 'f>);

impl Measure for Fonts<'_, '_, '_> {
    fn width(&mut self, text: &str, font: Font) -> i32 {
        self.0.measure(text, style(font))
    }

    fn height(&mut self, font: Font) -> i32 {
        match font {
            Font::Heading => 30,
            _ => 22,
        }
    }
}

fn style(font: Font) -> Style {
    match font {
        Font::Body | Font::Mono => Style::Body,
        Font::Strong => Style::Strong,
        Font::Heading => Style::Title,
    }
}

impl Lagoon {
    /// Goes to `url`, the current page kept to come back to.
    fn go(&mut self, url: String) {
        if let Some(shown) = &self.shown
            && shown.url != url
        {
            self.back.push(shown.url.clone());
            self.forward.clear();
        }
        self.open(url);
    }

    fn back(&mut self) {
        if let Some(url) = self.back.pop() {
            if let Some(shown) = &self.shown {
                self.forward.push(shown.url.clone());
            }
            self.open(url);
        }
    }

    fn forward(&mut self) {
        if let Some(url) = self.forward.pop() {
            if let Some(shown) = &self.shown {
                self.back.push(shown.url.clone());
            }
            self.open(url);
        }
    }

    fn reload(&mut self) {
        if let Some(url) = self.shown.as_ref().map(|s| s.url.clone()) {
            self.open(url);
        }
    }

    /// Fetches `url` (or shows the start page); the response's body comes
    /// in [`Lagoon::read`].
    fn open(&mut self, url: String) {
        self.loading = None;
        self.address = url.clone();
        if url == START {
            self.message.clear();
            self.show(url, Content::Html(oceans_html::parse(START_PAGE)));
            return;
        }
        let Some(net) = self.net else {
            self.message = String::from("Lagoon may not use the network on this system.");
            return;
        };
        // The part after `#` names a place in the page; it is not sent.
        let fetched = String::from(&url[..url.find('#').unwrap_or(url.len())]);
        match oceans_web::get(net, &fetched) {
            Ok(response) => {
                let head = response.head;
                let status = (!(200..300).contains(&head.status))
                    .then(|| alloc::format!("HTTP {} {}", head.status, head.reason()));
                let content_type = head.content_type().map(String::from);
                if let Some(kind) = &content_type
                    && !readable(kind)
                {
                    self.message = alloc::format!("Lagoon shows web pages and text, not {kind}.");
                    return;
                }
                self.message = String::from("Loading…");
                let url = if response.url.is_empty() {
                    url
                } else {
                    response.url.clone()
                };
                self.address = url.clone();
                self.loading = Some(Loading {
                    url,
                    response: Box::new(response),
                    body: Vec::new(),
                    content_type,
                    status,
                });
            }
            Err(problem) => self.message = problem.0,
        }
    }

    /// Takes what has come of the page; shows it once all has.
    fn read(&mut self) {
        let Some(loading) = &mut self.loading else {
            return;
        };
        let mut buffer = [0u8; 16 * 1024];
        let mut taken = 0;
        let mut failed = None;
        let mut cut = false;
        while taken < TICK_READ {
            match loading.response.read(&mut buffer, 0) {
                Ok(0) => break,
                Ok(got) => {
                    let room = MAX_PAGE - loading.body.len();
                    loading.body.extend_from_slice(&buffer[..got.min(room)]);
                    taken += got;
                    if got >= room {
                        cut = true;
                        break;
                    }
                }
                Err(problem) => {
                    failed = Some(problem.0);
                    break;
                }
            }
        }
        if !(cut || failed.is_some() || loading.response.ended()) {
            self.message = alloc::format!("Loading… {} KiB", loading.body.len() / 1024);
            return;
        }
        let Some(loading) = self.loading.take() else {
            return;
        };
        let text = oceans_html::decode_page(&loading.body, loading.content_type.as_deref());
        let plain = loading
            .content_type
            .as_deref()
            .is_some_and(|kind| kind.trim().to_ascii_lowercase().starts_with("text/plain"));
        let content = if plain {
            Content::Plain(text)
        } else {
            Content::Html(oceans_html::parse(&text))
        };
        self.message = match (failed, cut, loading.status) {
            (Some(why), _, _) => alloc::format!("The page stopped coming: {why}"),
            (_, true, _) => String::from("The page is cut short: Lagoon keeps 2 MiB of a page."),
            (_, _, Some(status)) => status,
            _ => String::new(),
        };
        self.show(loading.url, content);
    }

    fn show(&mut self, url: String, content: Content) {
        self.address = url.clone();
        self.scroll.reset();
        self.shown = Some(Shown {
            url,
            content,
            page: Page::default(),
            // Laid out on the next frame.
            width: -1,
        });
    }
}

/// Whether Lagoon shows what `kind` names.
fn readable(kind: &str) -> bool {
    let kind = kind.trim().to_ascii_lowercase();
    ["text/html", "text/plain", "application/xhtml+xml"]
        .iter()
        .any(|k| kind.starts_with(k))
}

/// What the user typed made an address: `https://` when no scheme is
/// given.
fn address_of(typed: &str) -> Result<String, &'static str> {
    let typed = typed.trim();
    if typed == START {
        return Ok(String::from(START));
    }
    if typed.starts_with("http://") || typed.starts_with("https://") {
        return Ok(String::from(typed));
    }
    if typed.contains("://") {
        return Err("Lagoon opens http:// and https:// addresses.");
    }
    if typed.contains(char::is_whitespace) || !typed.contains('.') {
        return Err("That is not an address (Lagoon has no search yet).");
    }
    Ok(alloc::format!("https://{typed}"))
}

fn frame(ui: &mut Ui<'_, '_>, app: &mut Lagoon) {
    if !app.started {
        app.started = true;
        match app.requested.take() {
            Some(url) => match address_of(&url) {
                Ok(url) => app.go(url),
                Err(why) => {
                    app.go(String::from(START));
                    app.message = String::from(why);
                }
            },
            None => app.go(String::from(START)),
        }
        *ui.focus = Some(PAGE);
        ui.changed = true;
    }
    if app.loading.is_some() {
        app.read();
        ui.changed = true;
    }
    ui.background(colour::window());
    let whole = ui.area;

    // The toolbar: Back, Forward, Reload, the address, Go.
    let bar = Rect::new(whole.x, whole.y, whole.w, TOOLBAR);
    ui.surface.fill(bar, colour::sidebar());
    ui.surface.fill(
        Rect::new(bar.x, bar.y + bar.h - 1, bar.w, 1),
        colour::line(),
    );
    let mut tools = Bar::new(Rect::new(whole.x + 12, whole.y + 11, whole.w - 24, 30), 8);
    let back = tools.left(36);
    let forward = tools.left(36);
    let reload = tools.left(72);
    let go = tools.right(56);
    let field = tools.rest(i32::MAX);
    let mut action = None;
    if ui.button_in(back, "‹", false) {
        action = Some("back");
    }
    if ui.button_in(forward, "›", false) {
        action = Some("forward");
    }
    if ui.button_in(reload, "Reload", false) {
        action = Some("reload");
    }
    ui.area = field;
    ui.y = field.y;
    let mut address = core::mem::take(&mut app.address);
    let entered = ui.text_field(ADDRESS, &mut address, "Type an address");
    app.address = address;
    if ui.button_in(go, "Go", true) || entered {
        action = Some("go");
    }
    match action {
        Some("back") => app.back(),
        Some("forward") => app.forward(),
        Some("reload") => app.reload(),
        Some("go") => match address_of(&app.address) {
            Ok(url) => {
                app.go(url);
                *ui.focus = Some(PAGE);
            }
            Err(why) => app.message = String::from(why),
        },
        _ => {}
    }
    if action.is_some() {
        ui.changed = true;
    }

    // The page.
    let view = Rect::new(
        whole.x,
        whole.y + TOOLBAR,
        whole.w,
        whole.h - TOOLBAR - STATUS,
    );
    let measure = (view.w - 2 * MARGIN - 12).clamp(120, MEASURE_MAX);
    if let Some(shown) = &mut app.shown
        && shown.width != measure
    {
        let mut fonts = Fonts(ui);
        shown.page = match &shown.content {
            Content::Html(doc) => oceans_html::layout(doc, measure, &mut fonts),
            Content::Plain(text) => oceans_html::layout_plain(text, measure, &mut fonts),
        };
        shown.width = measure;
    }
    keys(ui, app, view);
    let started = ui.begin_scroll(view, &app.scroll);
    let top = ui.y;
    let left = view.x + MARGIN + ((view.w - 2 * MARGIN - 12 - measure) / 2).max(0);
    let mut pointed = None;
    let mut clicked = None;
    if let Some(shown) = &app.shown {
        let page = &shown.page;
        for text in &page.texts {
            let (x, y) = (left + text.x, top + MARGIN + text.y);
            if y + text.height < view.y || y > view.y + view.h {
                continue;
            }
            let ink = if text.link.is_some() {
                colour::accent()
            } else {
                colour::text()
            };
            ui.text_at(x, y, &text.text, style(text.font), ink, view);
            if text.link.is_some() {
                let underline = Rect::new(x, y + text.height - 3, text.width, 1);
                if underline.y < view.y + view.h {
                    ui.surface.fill(underline, ink);
                }
            }
        }
        for rule in &page.rules {
            let y = top + MARGIN + rule.y;
            if y >= view.y && y < view.y + view.h {
                ui.surface
                    .fill(Rect::new(left + rule.x, y, rule.width, 1), colour::line());
            }
        }
        // A link pointed at, and one clicked.
        let at = |(px, py): (i32, i32)| {
            view.contains(px, py)
                .then(|| page.link_at(px - left, py - top - MARGIN))
                .flatten()
        };
        pointed = at(ui.input.pointer).map(String::from);
        if let Some(click) = ui.input.click
            && let Some(href) = at(click)
        {
            ui.input.click = None;
            clicked = Some((shown.url.clone(), String::from(href)));
        }
        ui.y = top + 2 * MARGIN + page.height;
    }
    ui.end_scroll(started, &mut app.scroll);
    if let Some((base, href)) = clicked {
        ui.changed = true;
        match oceans_html::url::resolve(&base, &href) {
            // A place in the same page: B1 stays where it is.
            Some(url) if same_page(&url, &base) => {}
            Some(url) => app.go(url),
            None => app.message = String::from("Lagoon follows only http:// and https:// links."),
        }
    }

    // The status line: what is happening, or the link pointed at.
    let status = Rect::new(whole.x, whole.y + whole.h - STATUS, whole.w, STATUS);
    ui.surface.fill(status, colour::sidebar());
    ui.surface
        .fill(Rect::new(status.x, status.y, status.w, 1), colour::line());
    let line = match (&pointed, app.message.is_empty()) {
        (Some(href), _) => href.clone(),
        (None, false) => app.message.clone(),
        (None, true) => app
            .shown
            .as_ref()
            .map(|s| match &s.content {
                Content::Html(doc) if !doc.title.is_empty() => doc.title.clone(),
                _ => s.url.clone(),
            })
            .unwrap_or_default(),
    };
    ui.text_at(
        whole.x + 12,
        status.y + 3,
        &line,
        Style::Body,
        colour::muted(),
        status,
    );
}

/// The page's keys, when the address field does not have the keyboard.
fn keys(ui: &mut Ui<'_, '_>, app: &mut Lagoon, view: Rect) {
    if *ui.focus == Some(ADDRESS) {
        return;
    }
    let mut others = Vec::new();
    let before = app.scroll.offset;
    for key in core::mem::take(&mut ui.input.keys) {
        match key {
            KEY_DOWN => app.scroll.offset += LINE,
            KEY_UP => app.scroll.offset -= LINE,
            KEY_PAGE_DOWN | b' ' => app.scroll.offset += view.h - LINE,
            KEY_PAGE_UP => app.scroll.offset -= view.h - LINE,
            KEY_HOME => app.scroll.offset = 0,
            KEY_END => app.scroll.offset = i32::MAX / 2,
            key if is_copy(key) => {
                if let Some(shown) = &app.shown {
                    let text = shown.page.plain_text();
                    ui.copy(&text);
                }
            }
            other => others.push(other),
        }
    }
    ui.input.keys = others;
    // `end_scroll` keeps the offset within the page.
    app.scroll.offset = app.scroll.offset.max(0);
    if app.scroll.offset != before {
        ui.changed = true;
    }
}

/// Whether `url` is `base` but for the part after `#`.
fn same_page(url: &str, base: &str) -> bool {
    let strip = |u: &str| String::from(&u[..u.find('#').unwrap_or(u.len())]);
    url.contains('#') && strip(url) == strip(base)
}

fn main(start: Start) -> i64 {
    let Some(directory) = Directory::from_start(&start) else {
        return oceans_ui::EXIT_BAD_START;
    };
    let mut app = Lagoon {
        net: directory.find("use", "net"),
        address: String::new(),
        shown: None,
        loading: None,
        back: Vec::new(),
        forward: Vec::new(),
        scroll: Scroll::default(),
        message: String::new(),
        requested: Some(String::from(directory.args().trim())).filter(|a| !a.is_empty()),
        started: false,
    };
    oceans_ui::run_resizable(
        &directory,
        "",
        WIDTH,
        HEIGHT,
        MIN_SIZE,
        Some(TICK_MS),
        &mut app,
        frame,
    )
}
