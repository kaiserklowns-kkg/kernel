# ADR-0099: Opening files in apps ("open with")

- Status: Accepted
- Date: 2026-10-09
- Depends on: ADR-0045 (Core), ADR-0059 (windows), ADR-0082 (Files),
  ADR-0084 (Text Editor), ADR-0090 (Image Viewer), ADR-0094 (Music),
  ADR-0095 (the user's input as consent)
- Part of Phase 10 (Alpha: basic apps).

## Context

Files showed the start of a text file and nothing more. To look at a
picture the user had to start Image Viewer and type its name. ADR-0082,
ADR-0084 and ADR-0090 each left "opening a file in an app" for later.

**What users expect, on every desktop:**
- **Windows:** a double click opens a file in the app registered for its
  extension; "Open with" lists the apps that can, and one can be chosen.
- **macOS:** apps declare the document types they open (Info.plist); a
  double click opens the default one; "Open With" lists the others. A
  sandboxed app gets access to just the file the user opened.
- **Linux (GNOME, KDE):** `.desktop` files declare MIME types; a double
  click opens the default (`mimeapps.list`); "Open With Other
  Application" lists the rest; Flatpak's document portal hands a sandboxed
  app only the file chosen.
- **Everywhere:** the app starts with the file already open.

**What matters for Oceans:** an app must not start other apps at will,
and opening a file must not hand an app more of the user's files than it
already has.

## Decision

**Apps declare what they open** (`opens` in the manifest):
- File name extensions, lowercase letters and digits, up to 16
  (`opens = png bmp`). The extension is what follows the name's last dot
  (not a leading one); it is matched in any case.
- Only an app with `window` and `files` may declare it, and no service
  or web app: being opened gives an app nothing it did not already have.
  An app without `files` that wants one file is a later decision (a
  per-file grant, like the macOS and Flatpak portals).
- Core reports the list (`INFO`, field `OPENS`).
- Text Editor opens plain text (`txt md log csv json toml ini conf cfg rs
  go sh`), Image Viewer `png bmp`, Music `wav`.

**Opening goes through the desktop** (the window end, `op::OPEN_FILE` and
`op::OPENERS`):
- The desktop already holds the user's end of Core and starts apps for
  the user; an app asks it, as for a copy (ADR-0095): once for each key or
  click the user gave its focused window. An app in the background cannot
  start anything.
- The name is a path in Home (`folder/name.png`): no `..`, no `.`, no
  empty part, no absolute path.
- `OPENERS` lists the apps that open such a file: the system's own first,
  then the others in Core's order. `OPEN_FILE` with no app chosen opens
  the first; with one, that one if it opens the file.
- The desktop starts the app with the file's name as its argument; if a
  permission is undecided, it asks first (ADR-0057) and then starts it
  with the same argument.
- It logs who opened what in which app (`desktop: opening picture.png
  with app.oceans.viewer (the default), for Files`).

**Files:**
- A click selects an entry, and shows the start of a text file; a second
  click opens it (a folder: into it; a file: in its app).
- Beside the list, Open, and Open with… which lists the apps that open
  the file to choose one, or says none does.

**Apps started with a name open it:** Text Editor now does, as Image
Viewer and Music already did. The toolkit gives every frame
`Ui::openers` and `Ui::open_file`.

**Versions:** Text Editor, Files and Image Viewer 1.0.3, Music 1.0.1, so
that existing disks install them.

## Consequences

- Files is a place to start from: pictures, songs and text open in their
  apps.
- Starting apps stays the user's act, through the desktop.
- **Not yet:**
  - a default the user chooses ("always open with"), kept by Core;
  - opening a file in an app that is already running (it says so);
  - per-file grants for apps without `files`;
  - kinds of file by their content, not their name;
  - opening from the Terminal (`app start ID NAME` still works).

## Alternatives considered

- **Core decides and starts, asked by any app with `files`:** any such app
  could start others whenever it liked; tying it to the user's input
  needs the desktop, which knows the focus.
- **MIME types (Linux, the web):** they need a table from names to types
  and back; extensions are what users see, and what apps can declare
  without one.
- **Opening by handing over a file handle:** the right design for apps
  without `files` (a portal); every app that opens files today has
  `files`, so a name is enough, and the handle is a later step.

## Checklist (master spec §48)

- **Purpose:** open a file in the right app, or one the user chooses.
- **Architecture:**
  - `opens`, `Manifest::opens`, `opens_file`, `extension` in
    `oceans-package`; `field::OPENS` in Core;
  - `op::OPEN_FILE`, `op::OPENERS`, `open_name`, `Manager::take_open` in
    `oceans-window`; the desktop's `openers` and `open_file`;
  - `open_file` and `openers` in `oceans-display-proto`; `Ui::openers`,
    `Ui::open_file`; Files, Text Editor.
- **API:** the manifest key `opens`; `INFO` field `OPENS`; the window
  ops `OPEN_FILE` and `OPENERS`.
- **Dependencies:** none.
- **Security:**
  - opening needs the user's key or click in the asking app's focused
    window, once each;
  - only apps with `window` and `files` open files, and only names in
    Home;
  - the app opened gets its own permissions, asked as usual; nothing
    more.
- **Testing:**
  - unit: `opens` parsed and refused (`oceans-package`), extensions; opening
    allowed once per input, separate from copying; names kept in Home
    (`oceans-window`);
  - smoke: in Files, picture.png clicked twice opens in Image Viewer (its
    picture on the screen); Open with… lists Image Viewer, and choosing it
    opens it again; the desktop logs both.
- **Failure behaviour:**
  - **No app opens the file:** Files says so; nothing starts.
  - **The app is running already, or cannot start:** the desktop says why
    in a notification.
  - **An open the user did not ask for:** refused (`NotAllowed`).
