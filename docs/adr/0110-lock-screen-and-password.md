# ADR-0110: The lock screen and the user's password

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0105 (UI direction: the lock screen and users), ADR-0045
  to ADR-0048 (Oceans Core and its rights), ADR-0057 (the desktop)
- Part of Phase 7 (UI) and Phase 10 (Alpha).
- First of the decisions on users; several users and each one's own data
  come later.

## Context

Anyone at the machine could use it: the desktop opened at start, and the
Terminal on it is the shell, which holds the system's rights. Nothing
could be left on the screen without it being open to whoever sat down.

**What users expect, on every system:**
- **Windows:** the sign-in screen at start; Win+L locks; the password
  (or a PIN) opens it; after several wrong ones, a wait before more.
- **macOS:** the login window, Control+Command+Q to lock, the password;
  waits after repeated wrong passwords.
- **GNOME and KDE:** a greeter at start, Super+L to lock, PAM's delays
  after failures.
- **Everywhere:** the password is never kept as such, only a slow hash;
  nothing of the session shows while locked.

## Decision

### The account, kept by Core

- **One user for now:** a name (`Oceans user` until named) and, if set,
  a password. Several users are a later decision.
- **The password is kept only as an Argon2id hash** (RFC 9106) with a
  random 16-byte salt and its parameters, in `/system/user`:
  `password argon2id m=7168 t=5 p=1 SALT HASH`. 7 MiB and five passes is
  one of the settings OWASP gives as equal to its first (19 MiB, two):
  the machine may have little memory free. The parameters are kept with
  each hash, so they can grow.
- **`oceans-account`** (host-tested): the hash and its check (constant
  time), the record's text, and the wait after wrong passwords.
  Dependency: the `argon2` crate (RustCrypto, MIT OR Apache-2.0).
- **A damaged `/system/user`** reads as a password that matches nothing:
  the screen stays locked rather than opening without one.

### Core's operations

- **`USER`** (op 27, `query`) → `[password set u8][name]`.
- **`SET_PASSWORD`** (op 28, `decide`): `[password]`, 4 to 64 bytes
  without control characters; empty removes it. Audited.
- **`SET_NAME`** (op 29, `decide`): 1 to 32 bytes.
- **`UNLOCK`** (op 30, `query`): `[password]` → `Ok`, or
  `WrongPassword`, audited with the count in a row. After four wrong ones
  in a row, each further one makes the next wait: 30 s, doubling, up to
  five minutes; `TooSoon` says how many seconds. Without a password, `Ok`.

### The lock screen, drawn by the desktop

- **At start** the desktop shows nothing of the session until Core says
  whether there is a password ("Starting Oceans..."): with one, the lock
  screen; without, the desktop.
- **Super+L** (`display::KEY_LOCK`, 0xaa, from both keyboards) locks, if a
  password is set; otherwise a note says how to set one.
- **While locked:** the time, the user's name and icon, a field showing a
  dot for each character typed, and what happened ("Wrong password: try
  again", "Too many wrong passwords: wait 30 s"). No window, the Terminal,
  notification or dialog shows; clicks reach nothing; keys go only to the
  field (the volume keys still work). Apps keep running.
- **Enter** sends the password to Core (`UNLOCK`); Escape clears it.
- **The shell:** `user` shows the account; `user name NAME`; `user
  password PASSWORD`; `user password --remove`.

## Consequences

- The machine can be left locked, and starts locked once a password is
  set.
- The smoke test names the user, refuses a short password, sets one,
  locks with Super+L, is refused a wrong password, opens with the right
  one, and starts locked after the reboot.
- **Not yet:**
  - several users, each with their own Home, apps' data and settings;
  - setting the password in Settings (today the shell);
  - locking after a while without input, and at sleep;
  - a lock button in the apps panel;
  - showing notifications on the lock screen (only that there are some);
  - **the serial console is not locked:** it is the machine's physical
    console, as a terminal on a serial line is on Linux; the screen's
    Terminal is;
  - the AI and the web experience hold `query` and may try passwords:
    the wait bounds them, and also delays the user after their tries.

## Alternatives considered

- **PBKDF2-HMAC-SHA256:** in the libraries already, but not memory-hard;
  graphics cards guess it far faster than Argon2id.
- **Argon2id at 19 MiB:** OWASP's first setting, but a fifth of a small
  machine's free memory for each try.
- **The desktop checking the password:** it would hold the hash; Core
  keeps it with the user's other settings, and the desktop only asks.
- **No wait after wrong passwords:** a guessing program could try as fast
  as the hash allows.

## Checklist (master spec §48)

- **Purpose:** nothing of the session open to whoever is at the machine.
- **Architecture:** Core keeps and checks; the desktop draws the lock
  screen and routes the keys; `oceans-account` holds the logic.
- **API:** `op::USER`, `SET_PASSWORD`, `SET_NAME`, `UNLOCK`;
  `Status::WrongPassword`, `TooSoon`; `display::KEY_LOCK`; the shell's
  `user`.
- **Dependencies:** `argon2` 0.5 (MIT OR Apache-2.0), no_std with `alloc`.
- **Security implications:** the password never leaves Core except as
  typed into the lock screen's field; it is kept only as a salted
  Argon2id hash; checks are constant time and slowed after failures;
  setting it needs the `decide` right; a damaged record keeps the screen
  locked; nothing of the session is drawn while locked.
- **Testing strategy:** host tests for the hash, the record, the bounds
  and the waits; the boot smoke sets a password, locks, is refused,
  unlocks, and starts locked after the reboot.
- **Failure behaviour:** without Core the desktop opens (no password can
  be set); if Core cannot be asked, it stays at "Starting Oceans..."; a
  failed check says so and leaves the screen locked.
