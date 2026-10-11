# ADR-0116: Each user's own settings

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0115 (several users), ADR-0113 (appearance and accent),
  ADR-0109 (apps kept in the dock), ADR-0114 (keyboard layouts)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

ADR-0115 gave each user their own Home and password, but the look of the
desktop, the apps kept in the dock and the keyboard layout were the same
for everyone: one user choosing dark made it dark for all, and the
layout was forgotten at every start.

**What users expect, on every system:**
- **macOS, Windows, GNOME:** the appearance, accent, Dock or taskbar, and
  input sources are each user's; signing in brings back one's own.
- **Shared by everyone:** the volume (a property of the speakers), the
  network, installed apps and what they may do.
- **Kept across restarts:** the input source last used.

## Decision

### Kept per account, by Core

- **The dock's apps** (`PINNED`, `PIN`), **the appearance and accent**
  (`APPEARANCE`, `SET_APPEARANCE`) and **the keyboard layout** (new:
  `LAYOUT`, op 40, `query`; `SET_LAYOUT`, op 41, `decide`; 0 English,
  1 Thai) are the account in use's.
- **Where:** the first account's in `/system/dock`, `/system/appearance`
  and `/system/layout`, as before (nothing moves); each other's beside
  them as `NAME.LOGIN` (`/system/dock.kim`). Removing an account removes
  these too, so a login used again starts afresh; its Home stays.
- **An account that chose nothing** gets the defaults: light, blue, an
  empty dock, English.

### Followed by the desktop

- **At start** it reads the first account's, as before; **at a sign-in**
  (ADR-0115) the new account's appearance (every window told, as in
  ADR-0113), dock and layout.
- **Super+Space and the menu bar's EN/TH** keep the layout chosen with
  Core: it is there again after a restart.

### The lock screen and dock, as seen running

Running Oceans with two accounts (Kai and Kim) showed what the smoke test
could not:

- **Two names with one initial looked the same:** both were a purple
  rounded "K", and the one name shown sat in the middle. Each account's
  picture is now a **circle in the colour of its place** among the
  accounts, with **each name under its own picture** (the chosen one
  bold, ringed in white).
- **The layout was not shown:** after Super+Space a password was typed
  in Thai with nothing to say so. The password field shows **EN or TH**
  at its right, as the menu bar does.
- **A layout chosen at the lock screen** is for typing the password
  only: it is not kept for the account in use, which may be about to
  change; signing in brings back the new account's.
- **Another user's dock showed the Terminal** they cannot open: it is
  shown dimmed.

### Still shared

- The volume, the network, installed apps, the decisions about their
  permissions, the services, and the time to lock after is already each
  account's own (ADR-0112).

## Consequences

- Each user signs in to their own look, dock and layout; the keyboard
  layout survives a restart.
- The smoke test signs Kim in and sees her apps panel light while Kai's
  stays dark, then signs Kai in and sees it dark again.
- **Not yet:**
  - each user's own app decisions and installed apps;
  - the volume per user (other systems share it too);
  - moving settings with the user to another computer.

## Alternatives considered

- **A folder per account in `/system`:** cleaner to list, but each name
  would need a directory made and walked; one file per setting, with the
  login after a dot, reads and writes as before.
- **Keeping settings in the user's Home:** apps allowed `files` could
  read and change them; `/system` is Core's alone.
- **Keeping the layout only in the desktop:** lost at every restart, and
  shared by every user.

## Checklist (master spec §48)

- **Purpose:** each user's own appearance, dock and keyboard layout, kept.
- **Architecture:** Core chooses the file for the account in use
  (`user_file`); the desktop reads them again at a sign-in.
- **API:** `op::LAYOUT`, `op::SET_LAYOUT`;
  `oceans_window::layout::Layout::{from_byte, byte}`; `PINNED`, `PIN`,
  `APPEARANCE`, `SET_APPEARANCE` now per account.
- **Dependencies:** none.
- **Security implications:** settings stay in `/system`, which only Core
  writes; changing them still needs `decide`; one account's never reach
  another's, and go when it goes.
- **Testing strategy:** host tests for the layout's byte; the boot smoke
  checks two accounts' appearances on screen across sign-ins.
- **Failure behaviour:** a missing or damaged file is the default
  (light, blue, an empty dock, English), as before.
