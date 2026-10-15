# ADR-0157: Lagoon, the web read (stage B1)

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0080 (the app toolkit), ADR-0135 (`oceans-web`),
  ADR-0138 (scrolling), ADR-0146 (the promises)
- Part of Phase 10 (Alpha).

## Context

Oceans can fetch over HTTPS (ADR-0135), but it has no way to read a web
page. The user asked for a browser named Lagoon, written for Oceans on
its own stack: Rust on `oceans-ui`, with no engine embedded. They also
asked that other browsers be surveyed first, so the scope stays fixed.

The survey is in
[docs/architecture/browser-survey.md](../architecture/browser-survey.md).
Chromium, Lightpanda and browser-use each need a POSIX host and a
JavaScript engine (or Chrome itself). Dillo, NetSurf and the text
browsers show what a small browser does first. B1 is the floor they
share: the web as text and links.

## Decision

- **`oceans-html`** (`libs/html`, `no_std` + `alloc`, tested on the host):
  - `token`: start and end tags with their attributes, and text. The
    character references used most are decoded, and numeric ones too.
    Comments and doctypes are skipped. `script`, `style`, `title`,
    `textarea`, `iframe` and similar hold raw text up to their own end
    tag. Nothing is an error: a `<` that starts no tag is text.
  - `tree`: a forgiving builder (after WHATWG HTML §13.2.6):
    - void elements never open;
    - a block closes an open `<p>`;
    - `li`, `dt`/`dd`, `tr`, `td`/`th` close their siblings;
    - an end tag with nothing open to match is dropped;
    - closing looks no further than a table cell;
    - scripts, styles, `svg`, `math`, `select`, media and frames are
      left out whole.

    The tree is at most `MAX_DEPTH` (128) deep: deeper elements go into
    the deepest one.
  - `layout`: blocks one under another, with collapsing margins:
    - headings (`h1`/`h2` in the heading font, `h3`–`h6` bold);
    - paragraphs, lists with bullets or numbers (`start=`), quotes
      indented, `dd`;
    - `pre` with its spaces and tabs; `hr`; `br`; an image's `alt` in
      brackets.

    Lines break between words. In Thai, which has no spaces, a line may
    break between clusters, never before a sign over or after a
    consonant and never after a leading vowel. A word wider than the
    line is cut. The glyph widths come from the caller (`Measure`). The
    result is text at positions, with its link, and the horizontal rules.
  - `url::resolve`: a link made whole against the page (RFC 3986 §5.2).
    Only `http` and `https` come out; `mailto:` and `javascript:` go
    nowhere.
  - `decode_page`: UTF-8, or TIS-620 / Windows-874 named by the
    `Content-Type` or a `<meta charset>` in the first KiB.
- **`oceans-web`:** `Response::url`, the address after redirects, so
  that relative links resolve against where the page really is.
- **Lagoon** (`app.oceans.lagoon`, a system app). Its permissions are
  `window` and `network`.
  - The window has Back, Forward, Reload, the address field and Go.
    `https://` is assumed when no scheme is typed, and anything that is
    not an address is refused (there is no search yet).
  - The page is set at most 760 pixels wide and centred, and laid out
    again when the window's width changes. Links are in the accent,
    underlined. The status line shows the link pointed at, what is
    happening, or the page's title.
  - It reads HTML and plain text. Anything else is refused by its
    `Content-Type`. A non-2xx page is shown, with its status line.
  - A page is read a piece at a time on the app's 100 ms tick (at most
    256 KiB a tick, 2 MiB in all), so the window answers while it comes.
    Only the connection and the head are waited for.
  - The keys: the arrows, Page Up/Down, Space, Home and End scroll.
    Ctrl+C copies the page's text when the address field does not have
    the keyboard.
  - It opens on a built-in start page, or on the address it was started
    with (`app start app.oceans.lagoon ADDRESS`).

## What the user sees change

Launchpad gains Lagoon, between Hello and Music, which moves the tiles
after it one place on. Web pages can be read and their links followed.
Nothing else moves.

## Consequences

- The smoke test serves Lagoon a page from the host behind a redirect.
  Lagoon, maximized, shows its first line, a heading that is a link,
  underlined in the accent. A click on it follows the relative link to
  the next page, whose link is a line lower. Core is expected to list 8
  apps (17 in the reboot).
- Most of today's web is built for scripts and styles. Many pages will
  show as their text in order, sometimes with menus first, as in Lynx.
  B2 (CSS 2.1, tables, images) and B3 (forms, bookmarks, history) are
  the next stages ([survey](../architecture/browser-survey.md)).
- Not in B1:
  - `#part` anchors (a link to a place in the same page does nothing);
  - `<base href>`;
  - keyboard navigation between links;
  - a page that changes while it is open;
  - gzip (the request asks for identity).
- The fetch's connection and head still block the window. ADR-0135's
  `get` does, as Music's does. Opening a connection without blocking
  is a change to `oceans-web` for later.

## Alternatives considered

- **Porting Chromium, Lightpanda or using browser-use.** Each needs a
  POSIX host and a JavaScript engine or Chrome itself (see the survey).
- **Rendering through the web bridge on another computer.** Oceans
  would not then read the web itself.
- **Laying a page out as it comes.** Lost for B1: a page is laid out
  once all of it has come (2 MiB at most, so this is quick). Showing it
  as it comes can be added when slow pages show the need.

## Checklist (master spec §48)

- **Purpose:** reading the web on Oceans.
- **Architecture:** a library tested on the host, plus an app on the
  toolkit.
- **API:** `oceans_html::{parse, layout, layout_plain, url::resolve,
  decode_page}`, and `oceans_web::Response::url`.
- **Dependencies:** none new.
- **Security:**
  - pages are untrusted, parsed without panics and bounded in depth and
    size;
  - no script runs;
  - Lagoon holds no `files` right;
  - HTTPS is checked, and an `https` page never redirects to `http`.
- **Testing:** host tests for tokens, references, the tree's recovery,
  layout positions, line breaking in English and Thai, links and plain
  text, URL resolution and TIS-620. Every prefix of a page is parsed and
  laid out. The smoke test is described above.
- **Failure behaviour:** a failed fetch is a message in the status line.
  A page that stops coming, or is cut short at 2 MiB, is shown as far as
  it came, and says so.
- **Promises (ADR-0146):**
  - nothing is fetched until the user asks: the start page is built in;
  - where the user goes is never written to a log;
  - the network the app uses is listed as its permission.
