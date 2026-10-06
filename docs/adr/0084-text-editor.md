# ADR-0084: Text Editor, and the keys that move

- Status: Accepted
- Date: 2026-10-06
- Depends on: ADR-0080 (the toolkit, apps that come with the system),
  ADR-0029 (the PS/2 keyboard), ADR-0059 (keys to app windows),
  ADR-0053 (`files`)
- Part of Phase 10 (Alpha: basic apps).

## Context

A system needs a way to write and change a text file without the shell.
An editor needs the cursor to move: the arrows, Home, End, Delete and the
page keys. Both keyboards (PS/2 in the kernel, the USB boot keyboard in
`oceans-usb`) sent only ASCII and dropped those keys.

## Decision

**The keys that move.** Keyboards send bytes past ASCII for them,
`oceans_abi::display::KEY_*`:

| Key | Byte |
|---|---|
| Up | 0x80 |
| Down | 0x81 |
| Left | 0x82 |
| Right | 0x83 |
| Home | 0x84 |
| End | 0x85 |
| Delete | 0x86 |
| Page Up | 0x87 |
| Page Down | 0x88 |

- They travel as key events do (ADR-0059); the desktop routes them like
  any key.
- What reads only text skips them: the shell takes 0x20–0x7e and its
  control bytes; the toolkit's text field and the example apps do the
  same. No ABI version changes, as nothing older breaks.

**Editing, `oceans-edit` (`libs/edit`):**
- a buffer of lines and a cursor;
- moved and changed by key bytes;
- UTF-8 throughout (a file may hold Thai: the cursor moves by characters);
- Up and Down keep the column they started from;
- a size limit;
- tested on the host.

**Text Editor** is an app on the toolkit and `oceans-edit`, brought by the
image, with `window` and `files`:
- **Files:** the name of a file in Home (`folder/name.txt` for one in a
  folder), Open, Save (also Ctrl+S) and New.
- **Untitled pages:** a page never named is saved as `untitled.txt`.
- **Limits:** files up to 256 KiB, UTF-8 only ("not a text file"
  otherwise).
- **The page:**
  - a click puts the cursor;
  - the view follows the cursor down and sideways;
  - the status bar shows the line, the column and whether there are
    changes.
- **Changes not saved:** Open and New lose them only on a second click.

**Room for the apps.** Every program that draws text carries the
interface fonts, and the boot archive stays in memory. With a fifth app
in the image, the smoke machine (256 MiB) left a Go example too little
memory to start. The Latin font (Noto Sans, 620 KB a weight, mostly Greek,
Cyrillic and Vietnamese) became a subset of what an English and Thai
interface uses: 47 KB a weight, about 1.1 MB less per program
(`libs/draw/fonts/README.md` gives the ranges and the command).

## Consequences

- Text files can be written and changed in a window.
- Apps can use the moving keys; the toolkit's one-line field can take
  them later.
- Text in scripts outside the subset (Greek, Cyrillic, Vietnamese) shows
  as missing glyphs until a font for it is chosen, as other scripts do
  today.
- **Not yet:**
  - selecting, copying and pasting;
  - undo;
  - search;
  - choosing a file from a list (Files does not open files in apps yet);
  - lines are not wrapped.

## Alternatives considered

- **Escape sequences (`ESC [ A`, as terminals send):** several bytes per
  key; Escape alone would then be ambiguous for apps. One byte per key
  keeps key events whole.
- **Control bytes (0x1c–0x1f):** too few for nine keys, and 0x1e is
  already the next window.

## Checklist (master spec §48)

- **Purpose:** editing text files; the keys an editor needs.
- **Architecture:**
  - `libs/edit` (`oceans-edit`), `user/apps/editor`;
  - key bytes in `oceans-abi`, `libs/usb` (`hid`) and the kernel's PS/2
    driver.
- **API:** `display::KEY_*` (additive); `oceans_edit::Editor`.
- **Dependencies:** none.
- **Security:**
  - only `/home`, through `files`;
  - the new bytes are ignored by everything that reads text.
- **Testing:**
  - unit: `oceans-edit` (typing, joining lines, columns, Thai, the
    limit), the USB keyboard's moving keys;
  - smoke: Text Editor typed into on the USB keyboard, Home, Ctrl+S, and
    the file read back with `cat`.
- **Failure behaviour:** a file that cannot be read or saved says why in
  the status bar; the page is kept.
