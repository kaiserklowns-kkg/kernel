# ADR-0077: Interface text: Noto Sans, and Thai

- Status: Accepted
- Date: 2026-10-06
- Amends: ADR-0057 (the desktop's text), ADR-0062 (the names the `oceans`
  tool accepts)
- Part of Phase 10 (Alpha).

## Context

The desktop drew all its text in a monospaced bitmap font that has only
Basic Latin. Anything else showed as `?`:
- an app called in Thai;
- a window titled in Thai;
- a notification in Thai.

Oceans' first users write Thai. Manifests already accept any printable
UTF-8 name (ADR-0046), but the `oceans` developer tool allowed only ASCII
names. Next to the taskbar and Start menu of ADR-0076, the monospaced font
also looked like a terminal, not a desktop.

## Decision

### Interface text in Noto Sans, Thai in Noto Sans Thai

- **The fonts:** Noto Sans and Noto Sans Thai, Regular and Bold, hinted
  TrueType.
  - They come under the SIL Open Font License 1.1, which allows bundling
    them with software.
  - They live in `user/display/fonts`, with `OFL.txt` and a README naming
    their source.
  - They are built into the display service: 1.3 MB.
- **Rasterizing:** `ab_glyph` (no `std`, with `libm`) rasterizes them,
  anti-aliased.
  - Each character is rasterized once per style (body, strong, title) and
    kept. Redrawing the desktop costs blending only.
- **Font choice per character:** Thai (U+0E00–U+0E7F) comes from Noto Sans
  Thai, everything else from Noto Sans. A character neither has shows as
  `?`.
  - Thai vowel and tone marks have no advance; the font places them over
    the letter before.
- **Measuring text:** the canvas measures text (`measure`). The desktop
  centres and fits with it: buttons, tiles, the clock.
- **The Terminal keeps the bitmap font.** It is a grid of console cells
  (bytes), and redraws thousands of characters.

### Thai names for apps

- **The `oceans` tool** accepts letters and digits of any script in a
  project's name and publisher, Thai with its marks included.
  - It still refuses what could break a template: quotes other than `'`,
    backslashes, braces, line breaks and other separators.
- **The smoke test** builds its Go app as "สวัสดี Go". Its output is checked
  as it travels through the SDK, the package, Core and the console.

## Consequences

- Thai and other Latin-script names, titles and notifications show
  correctly on the desktop, in a proportional typeface that matches the web
  experience.
- **Limits:**
  - **The Terminal and the kernel console** still show only Basic Latin.
    Their cells are bytes; UTF-8 in the console is later work.
  - **No shaping:** Thai marks are placed by their own offsets. A tone mark
    over an upper vowel can touch it. Full shaping (GPOS mark-to-mark) is
    later work.
  - **Other scripts** (Chinese, Arabic, …) are not bundled. Each needs its
    font, and Arabic needs shaping.
  - **Typing:** the keyboard path still carries ASCII. Thai input methods
    are later work.
- **The display service grows by 1.3 MB** in the boot archive.

## Alternatives considered

- **A bitmap font with Thai:** no Noto bitmap has Thai, and bitmaps look
  dated next to the web experience.
- **Subsetting the fonts:** smaller, but a tool and a step in the build for
  a 1.3 MB saving. Later, if size matters.
- **Shaping with a full engine (rustybuzz):** larger and not needed for
  legible Thai in short labels; it can come with UTF-8 in the console.

## Checklist (master spec §48)

- **Purpose:** legible, modern interface text, and Thai.
- **Architecture:**
  - `user/display/src/text.rs` (the typesetter, with its cache);
  - `Canvas::text` and `Canvas::measure`;
  - `Font::Mono` for the Terminal.
- **API:**
  - no ABI or protocol change;
  - the `oceans` tool accepts non-ASCII names.
- **Dependencies:**
  - `ab_glyph` (MIT/Apache-2.0, no `std`);
  - the Noto fonts (OFL 1.1).
- **Security:**
  - fonts are built in, never loaded from apps;
  - app-supplied text (names, titles, notifications) is only measured and
    drawn;
  - a character the fonts lack shows as `?`.
- **Testing:**
  - unit: the `oceans` tool's names (Thai, Latin with accents; refused
    separators and backslashes);
  - smoke: the Thai-named Go app's output, and the desktop's captures with
    the new text.
- **Failure behaviour:** if a bundled font failed to parse, text would fall
  back to the bitmap font.
