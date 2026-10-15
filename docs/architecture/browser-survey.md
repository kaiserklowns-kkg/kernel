# Browsers surveyed for Lagoon

Status: research note, 2026-10-15. Input for ADR-0157. These browsers
are studied for their scope and their order of work, not copied. Lagoon
is written in Rust on Oceans' own libraries (`oceans-ui`, `oceans-draw`,
`oceans-web`, `oceans-tls`). It embeds no engine.

## Why our own

The three projects the user proposed answer other questions:

| Project | What it is | Why not for Oceans |
|---|---|---|
| Chromium | The full web platform: Blink, V8, Skia, a multi-process sandbox | Tens of millions of lines of C++. It needs POSIX, threads, a GPU stack and a toolchain Oceans does not have. Porting it would be most of the project. |
| Lightpanda | A headless browser in Zig (with V8) for automation and scraping | Headless: it draws nothing. It needs V8 and a POSIX host. |
| browser-use | A Python library that lets an AI agent drive Chrome through Playwright | Not a browser: it needs Chrome and Python. Something like it belongs on Oceans' agent runtime later, over a browser Oceans has. |

Writing our own keeps the master spec's rules:

- Rust;
- capabilities: Lagoon holds `window` and `network`, nothing else;
- untrusted input parsed by code that never panics and is bounded.

The cost is that it will not show the modern web as Chrome does. The
stages below say how far it goes.

## What smaller browsers did first

| Browser | Language | What it shows | What we take |
|---|---|---|---|
| Dillo | C (FLTK) | HTML 4 and a CSS subset, images, no JavaScript; fast on small machines | **The B1 to B3 target.** A forgiving parser, block and inline layout, and later tables, images and forms. JS is not on the list. |
| NetSurf | C (own toolkit) | HTML 4, CSS 2.1, many image formats; JS is optional (Duktape) and mostly off | **The B2 target.** CSS 2.1's cascade and box model, done to the spec before anything newer. |
| Lynx, w3m, links | C | Text: headings, lists, links, tables (w3m) | **The B1 floor.** A page reads well with no styling at all. |
| Ladybird | C++ (its own engines) | The modern web, written from the specs in years of work | **The method.** The WHATWG HTML parsing algorithm and the CSS specs followed by name, with a test for each part. |
| Servo | Rust | Modern layout (parallel), WebRender, SpiderMonkey | **The crate split:** parser, style and layout as libraries tested on the host. Its size is beyond our scope. |

## Lagoon's stages

| Stage | In it | Not in it |
|---|---|---|
| **B1** (ADR-0157) | Tokenizer and a forgiving tree builder (void elements, paragraphs and list items closed implicitly, stray end tags dropped, scripts and styles left out). Block and inline layout: headings, paragraphs, lists, quotes, `pre`, `hr`, `br`, a `strong` font. Lines broken between words, and in Thai between clusters. Links: click, an address field, Back, Forward, Reload. HTTP and HTTPS with redirects. UTF-8 and TIS-620. Plain text. Scrolling with the wheel and the keys. Copying the page's text. | Everything below. |
| **B2** | A CSS 2.1 subset: selectors, the cascade, `display`, margins, colours, fonts' weight and size, `text-align`. Tables. Images (PNG, BMP, then JPEG and GIF). A `<base>` and anchors (`#part`). | Flexbox and grid beyond a fallback, animations, transforms. |
| **B3** | Forms (GET and POST, text fields, buttons, `select`). Bookmarks, history, a cache, cookies for the site you are on (never third-party ones). Find in page. | Plugins, extensions, WebRTC, WebGL, WebAssembly, video. |
| **Later, if at all** | A JavaScript engine, sandboxed in its own process: a decision of its own, with its own ADR. | — |

## Rules for every stage

- **Nothing fetched until the user asks.** No prefetch, no telemetry, no
  safe-browsing calls. The start page is built in.
- **Where the user goes is theirs.** Addresses are never written to the
  system log. History (B3) is kept in the user's own files.
- **The page cannot hurt the browser.** No panic on any input. The tree's
  depth is bounded. A page is capped in size (2 MiB in B1). Tests cut
  every page short at every byte.
- **No downgrade.** HTTPS certificates are checked against Mozilla's
  roots, and an `https` page's redirect never leads to `http`.
