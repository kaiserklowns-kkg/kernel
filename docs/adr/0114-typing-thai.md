# ADR-0114: Typing Thai, and keyboard layouts

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0029 (keyboard bytes), ADR-0077 (Noto Sans, with Thai),
  ADR-0080 (app toolkit), ADR-0084 (Text Editor), ADR-0108 (search),
  ADR-0110 (lock screen)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

Oceans draws Thai (ADR-0077) but could not type it: the keyboards send
the US layout's bytes, and every field took them as they are. A Thai user
could read their files' names and not search for them, or write a line
in Text Editor.

**What users expect, on every system:**
- **macOS, Windows, GNOME:** Thai Kedmanee (TIS 820-2538) as an input
  source beside English; a shortcut to switch (Ctrl+Space or
  Win+Space, Super+Space on GNOME); the layout in use shown in the menu
  bar or the taskbar (EN, TH), a click there switches too.
- **Everywhere:** the same keys type the same characters in every app,
  the lock screen and the search included.

## Decision

### Layouts (`oceans_window::layout`)

- **`Layout`:** English (the default) and Thai. `character(byte)` is what
  a key's US byte types in the layout: `None` in English, and in Thai for
  all but the printable keys (and the space, which stays a space).
- **Thai is Kedmanee,** unshifted and shifted, as the other systems have
  it: "wmp" types "ไทย", "4kKkwmp" "ภาษาไทย". Host tests check the table
  and its mapping.

### Switching

- **Super+Space** (`KEY_LAYOUT`, 0xab, from the PS/2 and USB keyboards)
  goes to the next layout, locked or not. The desktop logs it
  (`desktop: keyboard layout Thai`).
- **The menu bar** shows the layout's short name (EN, TH) left of the
  speaker; a click switches.

### Where the characters go

- **Windows:** the window protocol's `kind::TEXT` (event 9: `x`, `y` the
  low and high 16 bits of the character) comes to the focused window,
  instead of the key. `oceans-ui` takes it into its fields
  (`Input::text`), and Text Editor into the page: every app on the
  toolkit types Thai with no change of its own. Apps that do not know the
  event skip it.
- **The desktop's own:** the search in the apps panel, and the lock
  screen's password (counted as characters, shown as dots).
- **The Terminal** gets the US keys, as before: the console's font and
  the shell are ASCII.

## Consequences

- Thai is typed everywhere the toolkit draws a field, in Text Editor, the
  search and the lock screen.
- The smoke test switches to Thai, types "wmp" into the search, sees
  "ไทย" searched for, and switches back.
- **Not yet:**
  - other layouts (Pattachote, US International, Dvorak), and choosing
    which ones are offered;
  - the layout kept across reboots, and one per window;
  - Thai in the Terminal and the shell, and in Go apps' windows;
  - an input method for languages that need one (Chinese, Japanese).

## Alternatives considered

- **Mapping in the kernel's keyboard drivers:** each driver would keep
  the table and the state, and the bytes would stop being one byte; the
  desktop already knows which window the keys are for.
- **Mapping in each app:** every app would keep a layout and its own
  switch; the user expects one, everywhere at once.
- **Sending UTF-8 bytes as keys:** the bytes 0x80 and up are the moving
  and special keys (ADR-0084); a character in one event is unambiguous.

## Checklist (master spec §48)

- **Purpose:** typing Thai, with a switch between it and English.
- **Architecture:** the table in `oceans_window::layout`; the desktop
  keeps the layout in use and turns keys into characters; windows get
  `kind::TEXT`; the toolkit takes it into fields.
- **API:** `oceans_window::layout::{Layout, thai}`; `kind::TEXT`,
  `Event::character`, `Manager::text`; `KEY_LAYOUT`; `oceans_ui::Input`'s
  `text`.
- **Dependencies:** none.
- **Security implications:** the characters go only to the focused
  window, as keys do, and count as the user's input (the clipboard's
  rules, locking after a while); the lock screen takes the password in
  either layout, so a password typed in Thai opens only in Thai.
- **Testing strategy:** host tests for the table, the event's encoding
  and the focused window getting it, and the USB keyboard's Super+Space;
  the boot smoke types Thai into the search.
- **Failure behaviour:** a key with no Thai character types its US one;
  an app that ignores `kind::TEXT` gets nothing in Thai.
