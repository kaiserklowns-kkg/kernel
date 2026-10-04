# ADR-0059: Keyboard focus and app windows (ABI 14)

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0017 (console input), ADR-0032 (USB keyboards),
  ADR-0045 to ADR-0048 (Core, permissions), ADR-0056 (UI architecture),
  ADR-0057 (display service and desktop)
- Adds: ABI 14 (`DISPLAY_KEYBOARD`, `DISPLAY_KEYS`), the `window`
  permission, Core `WINDOWS` / `WINDOW_OWNER`

## Context

The desktop (ADR-0057) drew the screen, but nothing else could, and the
keyboard always typed into the console. Phase 7 needs two things:
- apps that show windows;
- a keyboard that goes to the window the user chose.

Both have to keep the desktop trustworthy:
- an app must not draw outside its window, read the keyboard while
  another window is in use, or pass itself off as another app (or as the
  system);
- the shell must stay usable with no desktop at all.

## Decision

### Keyboard (kernel, ABI 14)

- **`DISPLAY_KEYBOARD (display, notification, bits)`** — only the process
  holding the screen (`DISPLAY_CLAIM`) may call it, and it needs `MANAGE`.
  - From then on, keyboard bytes (the PS/2 keyboard, and USB keyboards
    through `CONSOLE_INPUT`) queue for it, up to 1024, instead of going to
    the console, and `bits` is signalled.
  - **Serial input still goes to the console:** it is the out-of-band
    line.
  - The holder's own `CONSOLE_INPUT` goes to the console: that is how the
    Terminal gets its keys.
- **`DISPLAY_KEYS (display, ptr, capacity)`** takes queued bytes without
  waiting.
- **When the holder exits,** the process's exit path gives the keyboard
  back to the console, as the screen is given back (ADR-0057). The queue
  lock is always taken with interrupts off, because the PS/2 interrupt
  feeds it.
- **Ctrl+Tab** sends `KEY_NEXT_WINDOW` (0x1e, ASCII RS) on both keymaps.
  No other key sends that byte, and the desktop never passes it on.

### The `window` permission and Core

- **`window`** ("show windows, and get what you type into them") is
  **automatic**, because:
  - a window gets keys and clicks only while the user gives it the focus;
  - the frame around it names the app.
- **The display service makes the window endpoint itself.** It cannot be
  an init `provide`: `use` must name an earlier service, and Core comes
  before the desktop. Instead:
  - the display service hands Core a server end with **only `MANAGE`**
    (`WINDOWS`), so Core can mint client ends but never receive;
  - Core mints a **badged** client end for each app started with
    `window` (`use windows`), and records the badge with the running app.
- **Who is behind a badge:** the display service asks Core with its own
  handle (`WINDOW_OWNER badge` returns `ID\0VERSION\0NAME` of the app
  running with it).
  - Calls go only from the display service to Core, so they cannot
    deadlock against the display's own calls to Core (`RUN`, `LIST`).
  - The display service never calls anything an app gave it.
  - An app cannot forge a badge, so it cannot pose as another.

### Windows (the display service; `libs/window`; `display-proto`)

- **Protocol:**
  - `OPEN (bits, width, height, title, [notification])` returns a window
    id and a shared memory object of `width × height` `0x00RRGGBB`
    pixels. The display maps it read-only.
  - `PRESENT (id)`, `EVENTS` (up to 20 events of 12 bytes), `CLOSE (id)`.
  - When the app's end closes (the app exits), all its windows close.
- **Limits:**
  - 64×32 to 1024×768 pixels;
  - titles of 48 bytes, with no control characters;
  - 4 windows per app and 16 on the screen;
  - all windows together hold at most twice the screen's pixels;
  - 64 queued events per app (pointer motion coalesces).
- **The frame is the system's:**
  - border and title bar: the app's **verified name**, then its own title
    in a muted style, and a close button;
  - the app draws only inside, and the system copies its pixels into the
    frame;
  - the focused window (or the Terminal) has the focus colour.
- **Focus:**
  - a new window takes the focus;
  - a click focuses and raises a window, and a click on the Terminal
    gives it the focus;
  - Ctrl+Tab goes round: the Terminal, then the windows in the order they
    were opened;
  - closing the focused window gives the focus to the window below, else
    to the Terminal.
- **What an app sees:**
  - keys while one of its windows has the focus;
  - pointer motion over its focused window;
  - clicks in its windows;
  - focus changes;
  - a click on its close button, as a request: the app closes the window.

  Dragging by the title bar is the system's.
- **A permission dialog is modal:**
  - keys go nowhere (typing must never land somewhere unseen);
  - windows get no clicks.
- **The policy is host-tested:** stacking, focus, dragging, hit-testing,
  limits and queues live in `libs/window` (`Manager`). The display service
  draws and moves bytes.

### The example: Notes

`app.oceans.notes` (`window`, `storage`):
- opens a 480×240 window;
- shows what is typed in it, and appends each line ended with Enter to
  `notes.txt` in its storage;
- ends on the close button.

## Consequences

- **Apps can have windows:** native Rust ones today, through
  `oceans-display-proto`.
- **The keyboard follows the user's choice.** The shell keeps working:
  - with the Terminal focused;
  - always through serial;
  - with the whole console back if the desktop stops.
- **Not yet:**
  - a Go binding for windows;
  - resizing;
  - keys beyond bytes (arrows, function keys, key releases);
  - pointer capture outside a window.
- **Smoke boots got longer,** so the smoke test allows 300 s per boot.
- **ADR-0058's open item is closed:** the bridge's permission catalog,
  copied from libs/package, is now checked against it by a test in
  libs/package.

## Alternatives considered

- **Windows as an init `provide`:** this needs the desktop to start before
  Core, but the desktop uses Core.
- **Core calling the display service to register each app:** the display
  service calls Core synchronously (`RUN`), so each would wait on the
  other.
- **Apps proving who they are with a handle or token:** the display would
  have to call into something an app gave it (an app could hang it), or
  keep secrets. Badges are kernel-enforced and need neither.
- **Asking consent for windows:** too many questions for little risk,
  given focus and frames. Revisit if windows get more power (a full
  screen, for instance).
- **Structured key events now:** the drivers and the console speak bytes.
  Scancodes come with a keyboard protocol of their own.

## Checklist (master spec §48)

- **Purpose:** apps with windows, and a keyboard focus the user controls.
- **Architecture:**
  - kernel keyboard diversion (ABI 14);
  - Core mints badged window ends and answers whose they are;
  - the display service frames, composes and routes;
  - `libs/window` holds the policy.
- **API:**
  - `DISPLAY_KEYBOARD`, `DISPLAY_KEYS`, `KEY_NEXT_WINDOW`;
  - the `window` permission;
  - Core `WINDOWS` and `WINDOW_OWNER`;
  - the window protocol (`OPEN`, `PRESENT`, `EVENTS`, `CLOSE`);
  - `oceans_display_proto::Window`.
- **Dependencies:** none new (Notes uses noto-sans-mono-bitmap, MIT, as
  the desktop does).
- **Security:**
  - keys reach only the focused window;
  - the frame names the app as Core verified it;
  - badges cannot be forged;
  - Core can only mint from the window endpoint;
  - every request is bounded;
  - the display reads pixels and never calls into an app;
  - keys are dropped during permission dialogs;
  - the keyboard returns to the console when the desktop dies.
- **Testing:**
  - unit: `libs/window` (13 tests: protocol bounds, placement, limits,
    ownership, focus cycling, raise, drag, close, pointer, queues), the
    USB keymap (Ctrl+Tab), and the bridge catalog check;
  - smoke: Notes is installed and clicked; its window opens with the
    focus; USB keys go to it and its line lands in its storage (not the
    shell); Ctrl+Tab gives the Terminal the focus; screen captures are
    checked for the window, its paper and each focus colour; the close
    button ends it; later USB keys reach the shell again.
- **Failure behaviour:**
  - no `console-input`: the keyboard stays with the console;
  - no Core: no windows;
  - an unknown badge: `NotAllowed`;
  - too many windows or too many pixels: `TooMany`;
  - an app that exits: its windows close;
  - the desktop that exits: the console gets the screen and keyboard.
