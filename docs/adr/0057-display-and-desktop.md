# ADR-0057: The display service and the desktop (ABI 13)

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0029 (framebuffer console), ADR-0042 (pointer input),
  ADR-0045 to ADR-0047 (Core, permissions), ADR-0056 (UI architecture)
- Adds: ABI 13 (`DISPLAY_INFO`, `DISPLAY_CLAIM`, `DISPLAY_TEXT`)

## Context

The kernel drew a text console on the boot framebuffer (ADR-0029), and
nothing else could draw. Phase 7 needs the screen in userspace: a desktop
with a launcher and system-rendered permission dialogs (ADR-0056). It
also needs the console as a fallback that can never be lost.

## Decision

### Kernel (ABI 13)

- **A `Display` object** exists when there is a usable 32-bit
  framebuffer. init receives it as **handle 5** and passes it on with
  `grant = display` (`READ`, `MANAGE`).
- **The calls:**

  | Call | Needs | Does |
  |---|---|---|
  | `DISPLAY_INFO` | `READ` | geometry, pixel format, the console's text grid |
  | `DISPLAY_CLAIM` | `MANAGE` | the framebuffer as device memory (mapped writable) |
  | `DISPLAY_TEXT` | `READ` | the console's text: header (columns, rows, cursor, a generation that changes with every write), then the cells |

- **Claiming:**
  - The console then **stops drawing but keeps its text**, so the shell
    keeps working and its output stays readable through `DISPLAY_TEXT`.
  - One live claimer at a time; others get `Busy`.
- **Taking the screen back:** when the claiming process is gone, the
  console takes the screen back at its next write, and redraws everything.
  A crashed desktop never leaves a blank screen.
- **Locking:** every holder of the display lock runs without preemption,
  as the logger does, because the logger takes it with preemption off. The
  exit of the claimer is reported by the process's exit path. Under the
  lock, the claimer is never upgraded to a strong reference, because
  dropping the last reference to a process logs.

### The display service (`user/display`, Rust)

It holds `display`, `use = core` (it is the user's agent, like the
shell), `use = input` and `sysinfo`.

- **Drawing:**
  - everything is drawn into a back buffer in memory;
  - **only pixels that differ from what is on the screen** are written
    to the uncached framebuffer;
  - text uses Noto Sans Mono (regular and bold, 16 and 20 px), the font
    the console already uses.
- **The desktop:**
  - **Design tokens:** dark first, neutral surfaces, one accent, and the
    semantic status colours of the master spec (§31).
  - **The Oceans Bar:** the clock and memory use.
  - **The launcher:** the installed apps from Core, with a dot for
    running ones.
  - **The Terminal window:** the console's text, ending at the cursor
    (the keyboard still types into the shell).
  - **Notifications.**
  - **The pointer:** relative mice and absolute tablets, through the
    input service.
- **The layout is anchored at the top left.** The launcher and the dialog
  keep their place on any screen of 900×420 or more, which also makes
  tests independent of resolution.
- **Launching:**
  - A click on an app starts it in the background (`RUN`, detached).
  - **If Core answers `NeedsConsent`,** the desktop shows a **permission
    dialog** for each undecided permission. It is drawn by the system with
    the app's verified identity, the permission's meaning in the system's
    words, and the app's reason as a quote, with Deny and Allow buttons.
  - The answer goes to Core (`DECIDE`, new source `DIALOG`, audited as
    "in a permission dialog"). Then the app starts with what was allowed.
- **Resetting a decision:** `DECIDE` gains "forget" (`allow = 2`), and
  the shell gains `app reset ID PERMISSION`, so the next run asks again.
  It is audited as "reset (ask again)", and a running app that held the
  permission is stopped, as for a revocation.

## Consequences

- **What works now:**
  - `cargo xtask run` opens a desktop;
  - apps start with a click;
  - consent is a real dialog;
  - the shell keeps working inside it.
- **Not yet:**
  - keyboard focus for windows (the keyboard types into the console);
  - windows for apps (surfaces apps draw into, a compositor protocol);
  - a proportional UI font;
  - the web layer on the device (ADR-0056).

  The protocol for app surfaces comes with the first graphical app.
  (Both keyboard focus and app windows came with ADR-0059.)
- **Speed:** every change redraws the back buffer, and only changed
  pixels reach the screen. A full first frame of 1280×800 is about 1 M
  uncached writes; later frames write what changed.

## Alternatives considered

- **Drawing in the kernel:** keeps policy (what the desktop shows) in
  the kernel, against ADR-0002.
- **Letting the display service read keystrokes now:** the console would
  have to share its input. That comes with keyboard focus, not before.
- **Mapping the framebuffer to apps directly:** any app could draw over
  a permission dialog.

## Checklist (master spec §48)

- **Purpose:** the screen in userspace: a desktop with launcher and
  permission dialogs, and the console as the fallback.
- **Architecture:**
  - the kernel `Display` object (ABI 13);
  - init's `display` grant;
  - the display service (back buffer, desktop, pointer, Core).
- **API:**
  - `DISPLAY_INFO`, `DISPLAY_CLAIM`, `DISPLAY_TEXT`;
  - `oceans_rt::display_*`;
  - `decision::FORGET`, `source::DIALOG`;
  - `app reset`.
- **Dependencies:** noto-sans-mono-bitmap (MIT), already in the kernel.
- **Security:**
  - one claimer at a time;
  - the console regains the screen when the claimer dies;
  - dialogs are drawn by the system;
  - decisions are audited with their source;
  - apps never get the framebuffer.
- **Testing (smoke):**
  - the desktop takes over the 1280×800 screen;
  - the launcher follows installs;
  - a USB mouse is hot-plugged; a click on Hello opens the dialog for
    its reset network permission; a click on Allow records the decision
    and starts it;
  - QEMU screen captures are checked on the host: bar, launcher and
    Terminal colours, then the dialog over the dimmed desktop.
- **Failure behaviour:**
  - no usable screen: the service exits and the console stays;
  - a busy screen: refused;
  - a dead claimer: the console redraws;
  - Core errors: shown as notifications.
