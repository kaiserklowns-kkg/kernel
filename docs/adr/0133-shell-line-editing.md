# ADR-0133: The shell's history and Tab completion

- Status: Accepted
- Date: 2026-10-13
- Depends on: ADR-0018 (the shell), ADR-0084 (the moving keys),
  ADR-0059 (the desktop's Terminal)
- Part of Phase 10 (Alpha).

## Context

On the first real machine every command was typed whole, and typed again
after each mistake. Commands like
`run fetch out use:net use:fs -- http://…` are long, and the shell
dropped the arrows. Every other shell has a history and completion.

## Decision

`oceans-line`, host-tested, without allocation:

- `History`: the last 32 lines entered (not empty ones, nor the newest
  again). Up walks back; Down walks forward and, past the newest, gives
  back what was being typed.
- `Matches`: the names that start with what was typed, offered one at a
  time. Its result is the rest of a single match plus a space (`/` for a
  directory), or the start every match shares.

In the shell:

- **Up and Down** recall lines: `display::KEY_UP` and `KEY_DOWN` from the
  desktop's Terminal, and `ESC [ A` / `ESC [ B` (or `ESC O`) from a serial
  terminal. Other escape sequences are still ignored.
- **Tab** completes the last word:
  - the first word from the shell's commands and `/bin`'s programs;
  - `run`'s program from `/bin`;
  - any other word as a path, from its directory.
  If several names match and share nothing more, they are listed and the
  line is shown again.
- Only command lines have them, not the answers to questions (consent,
  ADR-0047).

## Consequences

- Long commands are typed once and recalled; names are completed, not
  remembered.
- The smoke test types `ech` + Tab, Up and an addition, and a path + Tab
  over the serial line.
- Not yet: moving within the line (Left, Right, Home, End), a history kept
  across restarts, or completing `use:` grants and app IDs.

## Alternatives considered

- **Completion in the desktop's Terminal:** it does not know the shell's
  commands, paths or grants; the shell does.

## Checklist (master spec §48)

Purpose: typing less · architecture: a library and the shell's line
reader · API: none (keys already defined) · dependencies: `oceans-line` ·
security: completion lists only what the shell may already list ·
testing: host tests, the smoke over serial · failure: an unreadable
directory completes nothing.
