# ADR-0112: Locking after a while, and Lock in the apps panel

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0110 (the lock screen), ADR-0111 (the account in
  Settings)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

ADR-0110 locked the screen at start and on Super+L. A user who walked away
without pressing it left the session open, and one with a mouse alone had
no way to lock it.

**What users expect, on every system:**
- **Windows:** the screen locks after a while without input (with the
  screen saver, or Dynamic Lock); Lock is in the Start menu's account
  menu.
- **macOS:** "Require password after screen saver begins or display is
  turned off", the time chosen in Lock Screen settings; Lock Screen in the
  Apple menu.
- **GNOME:** Automatic Screen Lock after the screen blanks, its delay
  chosen in Privacy; Lock in the system menu.
- **Everywhere:** a few minutes by default, chosen by the user; a way to
  lock with the pointer.

## Decision

### Locking after a while

- **The time is the user's,** kept by Core with the account (`lock-after`
  in `/system/user`): never, or 1, 2, 5, 10, 15, 30 or 60 minutes; ten
  until chosen. It applies only with a password.
- **Core:** `LOCK_AFTER` (op 32, `query`) → `[minutes u8]`;
  `SET_LOCK_AFTER` (op 33, `decide`) `[minutes]`, one of the choices.
- **The desktop** notes the time of every key and pointer event it gets
  (not the serial console's), reads the setting with the account every
  two seconds, and locks once that long has passed without input
  (`oceans_account::lock_due`, host-tested). Apps playing sound or
  showing something do not keep it open.
- **Settings' Account:** under the password, "Lock the screen after this
  many minutes without input": Never and the choices, the one in use in
  the accent. The shell: `user lock-after [MINUTES | never]`.

### Lock in the apps panel

With a password set, the apps panel's footer has **Lock**, left of
Restart (or in its place on a desktop that cannot switch the machine
off): it locks as Super+L does.

Settings becomes 1.3.0.

## Consequences

- A session left alone locks itself; one click locks it too.
- The smoke test locks by the apps panel's button in the first boot, and
  in the second sets one minute, unlocks, and sees the screen lock again
  without input.
- **Not yet:**
  - turning the screen off (blanking, the display's power saving) before
    or with the lock;
  - locking at sleep or when the lid closes (there is no sleep yet);
  - keeping the screen open while a video plays (an app asking for it,
    as on other systems).

## Alternatives considered

- **Counting the serial console's input:** it is not at the screen, and
  someone typing there would keep the screen open for whoever sits at it.
- **The desktop keeping the time itself:** lost when the display service
  restarts, and not where the user's other settings are.
- **Any number of minutes:** a few fixed choices are what other systems
  offer, and what fits a row of buttons.

## Checklist (master spec §48)

- **Purpose:** the session locks when left alone, and by a click.
- **Architecture:** Core keeps the time; the desktop measures input and
  locks; Settings and the shell set it.
- **API:** `op::LOCK_AFTER`, `op::SET_LOCK_AFTER`, `LOCK_CHOICES`;
  `oceans_account::{lock_due, LOCK_CHOICES, DEFAULT_LOCK_AFTER}`; the
  shell's `user lock-after`.
- **Dependencies:** none.
- **Security implications:** changing the time needs the `decide` right;
  input the desktop does not see (the serial console, apps' own work)
  never keeps the screen open; without a password nothing locks, as
  before.
- **Testing strategy:** host tests for the record's new line and the
  lock's timing; the boot smoke clicks Lock, and waits for the lock after
  a minute without input.
- **Failure behaviour:** without Core the desktop keeps the last time it
  read (never, at first); a damaged time in the record makes the record
  damaged, which keeps the screen locked (ADR-0110).
