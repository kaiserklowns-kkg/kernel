# ADR-0119: The notifications, at the bell by the clock

- Status: Accepted
- Date: 2026-10-11
- Depends on: ADR-0065 (apps' notifications), ADR-0117 (the top bar),
  ADR-0118 (Control Center), ADR-0115 (several users)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

Apps allowed `notifications` (ADR-0065) could say something, and the
desktop showed it for a few seconds at the top right; then it was gone.
The design the user chose (ADR-0117) puts the time at the top bar's left,
as phones do, and the notifications under it, as a phone's notification
centre: what was missed stays until it is looked at.

**What users expect, on every system:**
- **iOS, Android:** a swipe from the top shows the time large, the date,
  and a card for each notification (the app, how long ago, what it said),
  newest first; a count shows what is new; Clear lets them go.
- **macOS:** the Notification Centre from the clock in the menu bar.

## Decision

- **Kept:** each notification an app sends is kept (its app's verified
  name, the text, the time), newest first, at most 30; the banner at the
  top right still shows it at once.
- **The bell opens them:** a bell in the top bar (the user asked for it,
  seen running: the time alone did not say it opens anything; since
  ADR-0121 at the bar's right end, as macOS has it); a click shows the
  panel under it:
  the time large and the date in words, "Notifications", and a card for
  each of the newest six (the app's icon and name, "now", "5 min ago",
  "2 h ago", the text), with "and N more" past six; "No notifications"
  when there are none.
- **What is new:** a red count on the bell of the notifications that
  came since the panel was last open; opening it clears the count.
- **Clear** lets them all go. A click outside, the bell again, Launchpad,
  Control Center or the lock screen closes the panel.
- **Each user's own:** when another user signs in (ADR-0115), the last
  one's notifications go: they may say what the next user should not see.
- **The time large:** a new text style, `Display` (Noto Sans regular at
  48 pixels, light as Big Sur's and phones' clocks), in `oceans-draw`
  and the display's `Font`; the panel's time uses it, and so does the
  lock screen, now the large time and the date in words above the
  accounts, as the chosen design has it.
- **Logged** for the smoke test: `desktop: notifications shown: 2 kept,
  the newest from Counter`, `desktop: notifications cleared (2)`.

## Consequences

- Nothing an app says is missed for looking away.
- The smoke test lets Counter notify, opens the panel from the bell,
  sees it in the dark appearance, and clears it.
- **Not yet:**
  - kept across restarts, and grouped by app;
  - acting on one (opening its app), and dismissing one alone;
  - Do Not Disturb, and choosing per app whether it may show banners;
  - the system's own messages (an app started, a volume error) stay
    banners only.

## Alternatives considered

- **Keeping them in Core:** across restarts and users, but every
  notification would cross another service; the desktop already holds
  them while they matter.
- **Keeping them per user across a switch:** the next user would see the
  last one's until a restart.

## Checklist (master spec §48)

- **Purpose:** what apps say, kept until looked at, from the bell by the clock.
- **Architecture:** the display service keeps `Desktop::notices` and
  `unread`; `bell_button`, `notice_panel`, `notice_card`, `notice_clear`
  place them; `clock_long` and `ago` word the time.
- **API:** `oceans_draw::Style::Display`; none new to apps (`NOTIFY` as
  before).
- **Dependencies:** none.
- **Security implications:** only what apps allowed `notifications`
  already showed; nothing leaves the desktop; a switch of user forgets
  them.
- **Testing strategy:** the boot smoke opens the panel after Counter's
  notification, checks its colour and the desktop's log, and clears it.
- **Failure behaviour:** past 30, the oldest go; without the time set,
  the panel shows no time.
