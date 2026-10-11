# ADR-0111: The account in Settings

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0110 (the lock screen and the user's password),
  ADR-0081 (Settings and system rights), ADR-0106 (the keyboard reaches
  every widget)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

ADR-0110 let the shell set the password (`user password`). A user who
never opens the Terminal had no way to set one, and a password set
through `SET_PASSWORD` needs no knowledge of the old one: anyone at an
unlocked desktop could change it.

**What users expect, on every system:**
- **macOS:** System Settings, Users & Groups: Change Password asks for the
  old password, then the new one twice.
- **Windows:** Settings, Accounts, Sign-in options: the current password,
  then the new one twice.
- **GNOME:** Settings, Users: the current password, the new one twice.
- **Everywhere:** the characters are never shown; a wrong current
  password changes nothing.

## Decision

### Changing needs the current password

- **`CHANGE_PASSWORD`** (op 31, the `decide` right): `[current length
  u8][current][new]`. Core checks the current one (empty when none is
  set) against its hash; wrong, it says `WrongPassword`, audits it, and
  counts it with the lock screen's wrong ones (ADR-0110's wait applies to
  both, `TooSoon`). Right, the new password is kept as `SET_PASSWORD`
  keeps it; empty removes it. Audited as changed with the current one.
- `SET_PASSWORD` stays for the shell, which holds the system's rights.

### Settings' Account section

A fifth section, **Account**:
- **The name:** shown; a field and Rename (Enter in the field too).
- **The password:** what it does now ("locks the screen at start and with
  Super+L", or "the screen is never locked"); the current password (when
  one is set), the new one, the new one again; Set Password or Change
  Password (Enter in the last field too), and Remove Password.
- The two new ones must match and have 4 to 64 characters; what happened
  is said under the buttons. What was typed is forgotten once it is used,
  right or wrong.

### A field for passwords (`oceans-ui`)

`Ui::password_field`: a text field that shows a dot for each character,
and cannot be copied or cut (pasting into it works). Like other fields it
has a role and a name (ADR-0106), so Tab reaches it.

Settings becomes 1.2.0.

## Consequences

- The password can be set and changed without the Terminal, and only by
  someone who knows it.
- The smoke test opens Settings, reaches Account and its fields with the
  keyboard alone (Tab, End), and changes the password.
- **Not yet:**
  - several users (each with their own Home and settings);
  - a hint for a forgotten password, and recovery without the shell;
  - showing the password while typing (an eye button);
  - checking how strong a new password is.

## Alternatives considered

- **`SET_PASSWORD` from Settings:** anyone at an unlocked desktop could
  replace the password and lock the owner out.
- **Asking for the current password only to remove it:** changing it to
  one only the changer knows is the same lock-out.
- **A separate throttle for changes:** two counters would let a guesser
  try twice as fast.

## Checklist (master spec §48)

- **Purpose:** the account's name and password, from Settings.
- **Architecture:** Settings draws and asks; Core checks and keeps;
  `oceans-ui` hides what is typed.
- **API:** `op::CHANGE_PASSWORD`; `Ui::password_field`.
- **Dependencies:** none.
- **Security implications:** the current password is needed and its tries
  share the lock screen's wait; passwords are never drawn, copied or
  kept by Settings after use; every change is audited.
- **Testing strategy:** the boot smoke changes the password in Settings by
  the keyboard, and Core's audit line shows it was checked.
- **Failure behaviour:** a wrong current password, a mismatch or a length
  out of bounds changes nothing and says why; without Core, the section
  says it is not available.
