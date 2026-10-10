# ADR-0106: The keyboard reaches every widget: focus, roles and names

- Status: Accepted
- Date: 2026-10-10
- Depends on: ADR-0080 (app toolkit), ADR-0105 (UI direction:
  accessibility is laid down now), ADR-0059 (keyboard focus between
  windows)
- Part of Phase 7 (UI) and Phase 10 (Alpha).

## Context

Inside a window, the keyboard reached only text fields and the areas an
app answered keys in itself (the editor's page, the viewer's picture). A
button, a list or Settings' sidebar could be used only with the mouse,
and nothing knew what the widgets were, so a screen reader could not be
added later without naming every widget again.

**What users expect, on every system:**
- **Windows:** Tab and Shift+Tab move through a window's controls; Space
  or Enter presses a button; the arrows move in a list; a focus
  rectangle shows where the keyboard is.
- **macOS:** the same with Full Keyboard Access (Tab, Shift+Tab, Space;
  a blue focus ring), and VoiceOver reading each control's role and name.
- **GNOME and KDE:** Tab moves the focus, arrows move in lists, and
  every widget has a role and a name (ATK, AT-SPI) for Orca.
- **Everywhere:** an app can be used without a pointer.

## Decision

### `oceans-access`: what a frame drew

`libs/access` (host-tested), for any immediate-mode UI:
- **[`Role`]:** heading, label, button, field, list, area (one the app
  draws and answers keys in itself).
- **[`Tree`]:** the widgets of one frame in the order drawn, each with an
  id, a role, a name, where it is, and whether the keyboard can use it.
- **[`Tree::next_focus`]:** where Tab (or Shift+Tab) takes the focus: the
  next (previous) widget that can be used, wrapping round; from nothing
  or from what this frame did not draw, the first (the last).
- **[`Ids`]:** an id for a widget the app does not name, from its role and
  name, the same in every frame; twins are told apart by their order. The
  top bit is set, so these never meet an app's own ids.

### `oceans-ui` uses it

- **Every widget is recorded** as it draws: headings, labels and rows by
  their text; buttons by their text; text fields by the app's id, named
  by their placeholder; lists and the sidebar.
- **Tab and Shift+Tab are the toolkit's:** taken out of the keys before
  the frame (no widget or app sees them), they move `ui.focus` after it.
  The focus is the same `ui.focus` text fields already used.
- **A focused button** is pressed by Enter or Space.
- **A focused list or sidebar:** Up, Down, Home and End choose a row (the
  list returns it, as a click on it would); Enter returns the selected
  row, as a second click does (Files opens it, Music plays it).
- **The focus ring:** two pixels in the accent just outside the focused
  button, the list's selected row, or the list itself when none is
  selected. Text fields keep their accent border.
- **[`Ui::focusable`]:** an area the app draws itself joins the order with
  the app's own focus id. The app shows that focus itself (the editor's
  cursor): no ring. The editor's page and the viewer's picture use it.

### Shift+Tab has a byte

`display::KEY_BACK_TAB` (0x8f), from the PS/2 and USB keyboards. Tab stays
`\t`; Ctrl+Tab stays the desktop's next window (ADR-0059), with or
without Shift.

### The apps

- **Calculator** reads the keys itself: while one of its buttons has the
  focus, it leaves Enter and Space to it.
- The others need no change: their widgets are the toolkit's.

## Consequences

- Every app on the toolkit can be used from the keyboard alone, and each
  frame says what its widgets are.
- The smoke test reaches Settings' sidebar with Tab, goes back and forth
  with Shift+Tab, and chooses Sound with Down.
- **Not yet:**
  - a screen reader (the tree is there; an accessibility service to read
    it from, and speech, are their own decisions);
  - the desktop's own parts (the menu bar, the dock, the apps panel,
    system dialogs) by keyboard;
  - Go apps' windows (`go/oceans/window` has no widgets);
  - names for lists (a list is named by nothing yet; a screen reader
    needs a label for it).

## Alternatives considered

- **A retained widget tree** (as in GTK or Qt): every app would be
  rewritten; recording what an immediate-mode frame drew gives the same
  tree.
- **Ids from the order drawn:** the focus would move to another widget
  whenever one appears above it.
- **Clicks give buttons the focus too** (as on Windows): a click on a
  button would take the keyboard from the text field the user types in.
- **Tab left to apps:** each would implement it, differently.

## Checklist (master spec §48)

- **Purpose:** the keyboard reaches every widget; each is known by its
  role and name.
- **Architecture:** `libs/access` (pure, host-tested) under `oceans-ui`;
  the keyboards' Shift+Tab byte in the kernel and `oceans-usb`.
- **API:** `Ui::tree`, `Ui::focusable`, `Ui::has_focus`, `Ui::finish`,
  `Role`; `display::KEY_BACK_TAB`. Existing calls are unchanged.
- **Dependencies:** none outside the repository.
- **Security implications:** none new: the keys stay within the window
  that has the keyboard; nothing leaves the app.
- **Testing strategy:** host tests for the focus order, the ids, the list
  keys and USB Shift+Tab; the boot smoke drives Settings with Tab,
  Shift+Tab and Down and checks the screen.
- **Failure behaviour:** a frame that draws nothing usable has no focus
  to move to; Tab then does nothing. A focus on a widget that went away
  moves to the first one on the next Tab.
