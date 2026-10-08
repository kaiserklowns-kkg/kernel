# ADR-0095: The clipboard: copied while focused, pasted only where the user pastes

- Status: Accepted
- Date: 2026-10-08
- Depends on: ADR-0059 (windows and the keyboard focus), ADR-0080 (the
  toolkit), ADR-0084 (Text Editor, the keys that move)
- Part of Phase 10 (Alpha: basic apps).

## Context

Nothing on Oceans could copy or paste. Text Editor had no selection, a
text field took only what was typed, and the Terminal took only keys.
Copy and paste is among the first things anyone does on a computer, so
before building it we looked at what users of the three desktops expect.

**What users expect, on every desktop:**
- **Windows:** Ctrl+C, Ctrl+X, Ctrl+V everywhere; Shift with the arrows,
  Home and End selects; a drag selects; Ctrl+A selects all. In Windows
  Terminal, Ctrl+Shift+C and V (Ctrl+C interrupts). The text stays after
  the app that copied it closes. Windows warns before a paste of several
  lines into the terminal.
- **macOS:** the same, on Cmd, so the terminal keeps Ctrl+C. Since macOS
  15.4 Apple has been bringing in an alert when an app reads the
  pasteboard without the user pasting; iOS already asks or shows "pasted
  from".
- **Linux:** Ctrl+C/X/V in apps, Ctrl+Shift+C/V in terminals. On X11 any
  client can read the clipboard at any time, and the text lives in the
  app that copied it: it is gone when that app exits. Wayland lets only
  the focused client set the selection, with the serial of a recent input
  event, and offers it only to the focused client.

**What this tells us:** the keys and the gestures are the same
everywhere, and users do not want to learn new ones. What differs is
trust. A clipboard any program can read when it likes leaks passwords
and private text (X11, Windows before 10, Android before 10); the newer
systems tie reading and writing to the user's own input.

## Decision

**The system keeps the clipboard** (text, in the display service):
- A copy is the system's own: it stays when the app that copied it ends.
- Up to 64 KiB of UTF-8, with no control characters but line breaks and
  tabs.
- The text is never logged; who copied, how much, and where it was pasted
  is (`display: clipboard: 16 bytes copied from Text Editor`).

**Copying** (`op::COPY` on the window end):
- Allowed only while the app's window has the focus, and once for each
  key or click the user gave that window (the focus moving on ends it).
- The text comes inline, or in a memory object the app shares; the
  display copies it before it looks at it.
- `oceans_display_proto::copy`, `Ui::copy` and Go's `window.Copy`.

**Pasting is pushed, never pulled:**
- No app can read the clipboard when it likes. When the user pastes
  (Ctrl+V or Ctrl+Shift+V) into a window, its app gets a `PASTE` event
  instead of the key, and `op::PASTE` hands it the text once, in a
  read-only memory object.
- The paste is for the app with the focus; it ends when the focus moves.

**The keys:**
- Ctrl+C, X and V stay the bytes 0x03, 0x18 and 0x16; in windows they
  copy, cut and paste.
- New key bytes (`oceans_abi::display`): `KEY_COPY`, `KEY_CUT`,
  `KEY_PASTE` for Ctrl+Shift+C, X, V and for the Copy, Cut and Paste
  keys some keyboards have; `KEY_SHIFTED` added to a moving key with
  Shift (Shift+Delete stays Delete).
- Both keyboards send them: the USB boot keyboard (`libs/usb`) and PS/2
  (the kernel). PS/2's "fake shift" around the moving keys is no longer
  taken for Shift.
- The shell and older apps skip bytes past ASCII, as before.

**The Terminal** pastes with Ctrl+Shift+V only (Ctrl+C interrupts, as on
Linux and Windows):
- It types the clipboard's first line, without control characters and at
  most 200 bytes (the shell's line).
- It never types Enter: a pasted line break would run a command the user
  has not read. A toast says when the rest was left out.

**Apps:**
- **The toolkit:** `Input::paste` carries what was pasted, `Input::held`
  says the main button is down. A text field takes a paste (its first
  line) and copies or cuts all of its text (fields have no selection).
- **`oceans-edit`:** a selection, from an anchor to the cursor. Shift
  with a moving key, `select_to` (a drag) and Ctrl+A make one; typing, a
  paste, Backspace and Delete replace it; Left and Right end it at its
  ends; `selected_text`, `cut`, `paste` (CR LF made LF). A paste that would
  pass the limit changes nothing.
- **Text Editor:** selects with Shift and the arrows, a drag or Ctrl+A,
  draws the selection, says how many characters are selected, and copies,
  cuts and pastes.
- **Go apps:** a `Paste` event kind, `window.Copy` and `window.TakePaste`.
- **Versions:** Text Editor and Image Viewer (which has a text field)
  become 1.0.1, so that a disk with the 1.0.0 apps installs the new ones
  from the image (only a newer version replaces an installed app).

## Consequences

- Copy and paste work in Text Editor, every toolkit text field, and the
  Terminal, with the keys users already know.
- No app learns what is on the clipboard unless the user pastes into it;
  an app in the background can neither read nor change it.
- **Not yet:**
  - other kinds than text (images, files);
  - copying from the Terminal (it has no selection);
  - a clipboard history (Win+V);
  - a selection in toolkit text fields;
  - Shift with a click (pointer events carry no modifiers);
  - drag and drop.

## Alternatives considered

- **A `clipboard` permission, asked:** users do not think of copy and
  paste as a right to grant, and a prompt per app would be clicked
  through. Tying it to the user's own keys gives the protection without
  the question.
- **A read call any focused app may make (Wayland's offer):** a focused
  app could still read the clipboard without a paste, as macOS now warns
  about. A paste the user made is the only time an app needs the text.
- **The app keeping the text until asked (X11's selections):** the text
  dies with the app, and the system must call into apps it does not
  trust.
- **Cmd-like Super key for copy and paste:** keyboards on Oceans' target
  machines are PC keyboards; Ctrl is what Windows and Linux users type.
- **Typing a multi-line paste into the Terminal:** a hidden line break
  runs commands ("pastejacking"); one line, never Enter, is safe.

## Checklist (master spec §48)

- **Purpose:** copy and paste of text between apps and into the Terminal;
  selecting text in Text Editor.
- **Architecture:**
  - the policy in `oceans-window`'s `Manager` (host-tested);
  - the text moving in the display service (`user/display/src/windows.rs`);
  - the keys in `oceans-abi`, `libs/usb/src/hid.rs` and
    `kernel/src/arch/x86_64/keyboard.rs`;
  - selection in `oceans-edit`; the toolkit and Text Editor.
- **API:**
  - `op::COPY`, `op::PASTE`, `kind::PASTE`, `MAX_CLIPBOARD`,
    `clipboard_text`;
  - `oceans_display_proto::{copy, paste}`, `Ui::copy`, `Input::paste`;
  - `display::{KEY_COPY, KEY_CUT, KEY_PASTE, KEY_SHIFTED, unshifted, is_copy,
    is_cut, is_paste}`;
  - Go: `window.Copy`, `window.TakePaste`, `window.Paste`.
- **Dependencies:** none.
- **Security:**
  - copy needs the focus and the user's input; paste only to the app
    pasted into, once;
  - shared memory is copied before it is checked; sizes are bounded;
  - the text is never logged;
  - the Terminal never receives a line break from the clipboard.
- **Testing:**
  - unit: who may copy and when, a paste taken once and only by the
    focused app, the clipboard outliving its app, the Terminal's one line
    (`oceans-window`); the new key bytes (`oceans-usb`); selections,
    cutting, pasting and the limit (`oceans-edit`); Go's `Copy` refusals
    and the event kinds;
  - smoke: in Text Editor, Shift+End, Ctrl+C, Enter, Ctrl+V and Ctrl+S
    save the copied line twice; Ctrl+Shift+V in the Terminal types it into
    `echo`, which prints it.
- **Failure behaviour:**
  - **A copy not allowed:** refused (`NotAllowed`); the clipboard keeps
    what it had.
  - **A paste with nothing on the clipboard:** nothing happens.
  - **A paste too large for the page:** Text Editor says so and changes
    nothing.
  - **No memory for the text:** the paste is refused (`NoMemory`).
