# ADR-0115: Several users, each with their own Home

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0110 (lock screen and password), ADR-0111 (account in
  Settings), ADR-0112 (locking after a while), ADR-0047 (permissions:
  `files`), ADR-0105 (UI direction: lock screen and users)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

Oceans had one account. A family or a shared office computer had to share
a password and one Home: everyone saw everyone's files, and anyone who
could unlock it could change everything.

**What users expect, on every system:**
- **macOS:** Users & Groups: an administrator adds standard users, each
  with a password and a home folder; the login window shows each user's
  picture; only administrators change the system.
- **Windows:** Other users in Settings, added by an administrator; the
  sign-in screen lists the accounts; each has their own profile folder.
- **GNOME:** Users in Settings, unlocked by an administrator; the login
  screen lists the users.
- **Everywhere:** one user's files are not the others'; switching users
  needs that user's password.

## Decision

### The accounts (`oceans-account`)

- **`Users`:** up to eight accounts, the first first. The first (id
  `owner`) **administers**; each one added has a **login** (1 to 16 of
  `a`–`z` and `0`–`9`, a letter first: it names a folder), a name, a
  password (always: one without could be signed in as by anyone) and its
  own time to lock after.
- **Kept in `/system/user`,** the first account's lines as before (an
  older Oceans reads them), then `user LOGIN` and each other's lines. The
  one account kept before reads as the first; damage anywhere is damage
  (the screen stays locked).
- **Homes:** the first account's is `/home`, as before, so nothing moves;
  each other's is `/users/LOGIN`, made when the account is added.

### Core

- **The account in use** (the first at boot) is the one `USER`,
  `SET_NAME`, `SET_PASSWORD`, `CHANGE_PASSWORD`, `UNLOCK` and
  `LOCK_AFTER` act on; apps allowed `files` get **its Home**, and `FIND`
  searches it.
- **`USERS`** (op 36, `query`) → each account's login, name and flags
  (administrator, password, in use).
- **`ADD_USER`** (op 37, `decide`) and **`REMOVE_USER`** (op 38):
  only while the administrator is in use and has a password
  (`NotAdministrator`, status 17). Removing keeps the account's files;
  the first account and the one in use cannot go.
- **`SIGN_IN`** (op 39, `decide`): an account's login and password,
  checked and slowed after wrong ones as `UNLOCK` is (one wait for all
  the accounts). Another account than in use first **stops the apps**:
  they hold the last user's Home and what they were shown. Services keep
  running, but those given Home start again with the new one's.
- With other accounts, no password can be removed.

### The desktop

- **The lock screen** shows each account's picture in a row, the chosen
  one ringed and named; Left and Right or a click choose, and the
  password signs that account in (`desktop: Kim signed in`).
- **On a switch:** the clipboard is emptied; the dock's apps, the
  appearance and the apps panel are read again.
- **The Terminal** is the system's console, with every right: it is the
  administrator's only. Another user's keys never reach it, and the dock
  says so.

### Choosing

- **Settings' Users** (Settings 1.5.0): each account and its kind; the
  administrator adds one (login, name, password) and removes others.
- **The shell:** `user list`, `user add LOGIN PASSWORD NAME`,
  `user remove LOGIN`.

## Consequences

- A computer is shared without sharing files: each user's apps see only
  their Home, and getting into another account needs its password.
- The smoke test adds Kim, signs her in from the lock screen (the
  Calculator stops, her search finds nothing of Kai's Home), goes back
  to Kai, and removes her.
- **Not yet:**
  - each user's own appearance, dock, volume and app decisions (shared
    for now), and their own apps;
  - several users signed in at once (fast user switching), and a guest
    account;
  - more administrators, and handing the role over;
  - files kept from each other by the filesystem itself: Homes are kept
    apart by the handles Core gives, and the system's own programs (the
    shell, the AI service) still reach every one;
  - removing an account's files with it, and pictures chosen by users.

## Alternatives considered

- **Moving the first account's files into `/users/owner`:** every path
  that names `/home` would break, and a move cut short could lose files;
  its Home stays where it is.
- **Keeping apps running across a switch:** they hold the last user's
  Home and show their windows; other systems keep a session per user, a
  much larger change (see Not yet).
- **Accounts without passwords:** the lock screen would let anyone choose
  one; with more than one account, each has a password.

## Checklist (master spec §48)

- **Purpose:** several people on one computer, each with their own files
  and password.
- **Architecture:** the accounts in `oceans-account`, kept by Core, which
  chooses the Home it gives; the desktop's lock screen signs in; Settings
  and the shell administer.
- **API:** `oceans_account::{Users, OWNER, MAX_USERS, valid_id,
  Record::{admin, home}}`; `op::USERS`, `ADD_USER`, `REMOVE_USER`,
  `SIGN_IN`, `Status::NotAdministrator`; `Manager::clear_clipboard`;
  `proto::KEY_LEFT`, `KEY_RIGHT`; the shell's `user list | add | remove`.
- **Dependencies:** none.
- **Security implications:** switching needs the account's password, slowed
  after wrong ones; only the administrator, with a password, adds or
  removes accounts; logins are checked before they name a folder; apps
  stop at a switch and the clipboard empties, so nothing of one user
  reaches the next; the Terminal is not another user's.
- **Testing strategy:** host tests for the accounts' text, logins and
  Homes; the boot smoke adds an account, signs it in from the lock screen,
  sees the apps stopped and its Home separate, goes back and removes it.
- **Failure behaviour:** a damaged `/system/user` keeps the screen locked,
  as before; a Home that cannot be made refuses the new account; a
  service that cannot start again after a switch is logged.
