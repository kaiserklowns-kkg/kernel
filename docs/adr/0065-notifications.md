# ADR-0065: Notifications for apps

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0047 (consent), ADR-0057 (the desktop's
  notifications), ADR-0059 (the display end), ADR-0062 (the SDK)

## Context

The master spec's SDK lists a Notification API (§40). The desktop already
showed notifications, but only its own ("Started Notes", errors).

Notifications from apps are a channel for phishing ("your session
expired, type your password in the Terminal") and for noise. They need:
- a decision by the user;
- an unforgeable sender;
- a limit on how often.

## Decision

- **A new permission, `notifications`** ("show you notifications on the
  desktop"). It is **not automatic**: the user is asked, as for the
  network.
- **One display end per app.** Core mints it if the app was granted
  `window` or `notifications` (ADR-0059). `WINDOW_OWNER` now answers which
  of the two it carries (`[grants u8]`, `display_grant` bits), and the
  display service enforces each: `OPEN` needs `window`, `NOTIFY` needs
  `notifications`.
- **`NOTIFY` (window protocol op 5):**
  - text of one line, 1 to 120 bytes, no control characters;
  - at most one every 3 s per app (`TooMany` sooner);
  - shown as **"App name: text"**, the name as Core verified it, in the
    system's style;
  - logged.
- **APIs:**
  - Rust: `oceans_sdk::notification::notify(&directory, text)`
    (`oceans_display_proto::notify`);
  - Go: `window.Notify(windows, text)`.

  The Rust template now notifies how many times it ran.
- **Web apps cannot notify.** They run in the paired browser, and a
  notification there would need a permission of its own.

## Consequences

- Apps can tell the user something without a window, only once allowed,
  only under their own name, and at a bounded rate.
- Notifications disappear after their 4 s like the desktop's own. A
  notification centre (history) is later work.

## Alternatives considered

- **Automatic, like `window`:** a window needs the user's focus to matter,
  but a notification appears on its own.
- **A separate notification endpoint:** another badge and registration for
  what the display end already identifies.

## Checklist (master spec §48)

- **Purpose:** the Notification API.
- **Architecture:** a permission; one display end with grants; a display
  op.
- **API:** the `notifications` permission; `display_grant`; `NOTIFY`;
  `notification::notify`; `window.Notify`.
- **Dependencies:** none.
- **Security:**
  - consent;
  - the verified name is prefixed;
  - one short line;
  - rate-limited;
  - logged.
- **Testing:**
  - unit: the text rules;
  - smoke: the SDK's Rust app asks for `notifications` at its first run;
    it is allowed and the desktop shows "Counter: ran 1 times".
- **Failure behaviour:**
  - not granted: `NotAllowed`;
  - too soon: `TooMany`;
  - bad text: `BadRequest`;
  - the template ignores failures.
