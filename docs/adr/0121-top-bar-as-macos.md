# ADR-0121: The top bar as macOS has it, and the user's menu

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0117 (the top bar), ADR-0118 (Control Center),
  ADR-0119 (the notifications), ADR-0115 (several users)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

Seen running, the user found the top bar unfriendly: the bell beside the
clock at the left and the status icons at the right were split, unlike
any system people know. They asked for macOS's arrangement, showing its
menu bar's right side (sound, the user, the battery, Control Center),
with the notifications at the bell, and the time kept at the left.

## Decision

- **The left:** the time and date, then the app in use, bold.
- **The right, from the end in, as macOS has it:** the **bell** (the
  notifications, ADR-0119, its count of new ones on it), **Control
  Center** (ADR-0118), the **network**, the **user**, the **speaker**,
  the **keyboard layout**. The battery takes its place between the
  network and Control Center once Oceans can read one.
- **The notifications' panel** opens under the bell, at the right.
- **The user's menu** (as macOS's Fast User Switching menu): the user's
  icon opens it under itself: the account's name, then **Lock Screen**
  (with a password), **Switch User...** (with other accounts: the lock
  screen, where the accounts are chosen; `desktop: switching user`) and
  **Users & Accounts...** (Settings). A click outside or on another icon
  closes it.
- Control Center opens from its own icon, the network's and the
  speaker's.

## Consequences

- One group of icons at the right, in the order PC users know from
  macOS; the time where the user asked for it.
- The smoke test switches user from the user's menu, and finds the
  speaker, the bell and the notifications' Clear at their new places.
- **Not yet:** the battery (as ADR-0118 says), the app's own menus
  beside its name, and a picture for each account.

## Alternatives considered

- **The bell at the left, beside the clock (ADR-0119 as first made):**
  turned down, seen running: two groups of icons for one bar.
- **The user's items in Control Center:** macOS keeps them apart, and the
  user asked for its arrangement.

## Checklist (master spec §48)

- **Purpose:** a top bar arranged as people know it, and the user's
  account one click away.
- **Architecture:** `status_icon` with `ICON_BELL`, `ICON_CONTROL`,
  `ICON_NETWORK`, `ICON_USER`, `ICON_SPEAKER`; `user_menu`,
  `user_menu_item`, `Desktop::user_items`, `UserAction`, `Hit::User`,
  `Hit::UserItem`.
- **API:** none new to apps.
- **Dependencies:** none.
- **Security implications:** none new: locking and the lock screen as
  before; Settings opens through Core.
- **Testing strategy:** the boot smoke uses Switch User, the bell's panel
  and Control Center at their places.
- **Failure behaviour:** without a password, no Lock Screen; with one
  account, no Switch User.
