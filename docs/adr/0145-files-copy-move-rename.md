# ADR-0145: Copy, move and rename in Files

- Status: Accepted
- Date: 2026-10-14
- Depends on: ADR-0082 (Files), ADR-0038 (rename), ADR-0039 (copying
  through a shared buffer)
- Part of Phase 10 (Alpha).

## Context

Files could open, create a folder and delete. Moving a file, renaming
one or making a copy meant the shell (`mv`, `cp`). The filesystem has
supported both for a long time: `RENAME` moves within a volume
atomically, and `tree::Copier` copies files and whole trees through one
shared buffer.

## Decision

- The panel of the entry selected (a file or a folder) has **Rename**,
  **Copy** and **Cut**, below the buttons already there (Open, Open
  with…), so these keep their places.
- **Rename** shows a field with the name. Enter renames the entry in its
  folder. A name already taken is refused, never replaced (`RENAME` would
  replace a file).
- **Copy** and **Cut** hold the entry (its path from Home). **Paste**
  appears in the toolbar while something is held, and acts on the folder
  shown:
  - Cut moves the entry there with one `RENAME` through Home, so it is
    atomic. Pasting into the folder it came from does nothing.
  - Copy copies it there with `tree::Copier` (256 KiB a request; a
    folder with all that is in it).
  - A name already taken gets `NAME copy`, then `NAME copy 2`, and so
    on. For a file the suffix goes before the extension:
    `tone copy.wav`.
  - A folder pasted into itself, or into a folder inside it, is refused.
- Messages under the list say what happened, or why not.

## Consequences

- The smoke test clicks tone.wav, then Copy and Paste in the same folder.
  The shell's `ls /home` then shows `tone copy.wav`. The screen is
  probed for the selection, and for Paste in the accent colour while
  something is held.
- A large copy holds the window until it is done (no progress or
  cancel yet).
- Not yet:
  - drag and drop;
  - more than one entry at a time;
  - the clipboard shared with other apps (ADR-0095 holds text only);
  - USB sticks in Files (it has `files`, Home only).

## Alternatives considered

- **Copying through the app's memory:** slower, and bounded by the app's
  heap. The shared buffer moves the bytes between the services.

## Checklist (master spec §48)

- Purpose: everyday file handling without the shell.
- Architecture: the Files app only.
- API: none (`RENAME`, `tree::Copier`).
- Dependencies: none.
- Security: inside Home (`files`), as before.
- Testing: the smoke test.
- Failure: refused moves and copies are reported, and nothing is
  replaced.
