# ADR-0082: Files

- Status: Accepted
- Date: 2026-10-06
- Depends on: ADR-0080 (the toolkit, apps that come with the system),
  ADR-0053 (`files`: the user's files in `/home`)
- Part of Phase 10 (Alpha: basic apps).

## Context

A system needs a way to look at and tidy one's files without the shell.
The basic apps (ADR-0080) begin with Calculator and Settings; Files comes
next.

## Decision

Files is an app on the toolkit, brought by the image. Its permissions are
`window` and `files` (the user's files in `/home`, nothing else). As a
system app it is not asked for them (ADR-0081).

- **The folder shown:**
  - folders first, then files with their sizes;
  - the path from Home;
  - Back to go up.
- **Opening:** a click selects; a second click on the selection opens a
  folder, or shows the start of a file (4 KiB, as text; "not a text
  file" otherwise).
- **New folder:** a name, then Enter. A name with `/`, `.` or `..` is
  refused.
- **Delete:** after a confirmation that names what goes. A folder goes
  with everything in it.
- **Durability:** changes are synced at once.

## Consequences

- Files can be browsed and tidied without the shell.
- **Not yet:**
  - copying, moving and renaming;
  - opening a file in an app;
  - the USB stick and other disks (`/usb`, `/nvme`, `/sata`): `files`
    reaches only `/home`, and a grant for removable media is its own
    decision;
  - folders of more than 300 entries are shown in part.

## Alternatives considered

- **Giving Files the whole file system:** the user's files are what Files
  is for; system files stay out of reach, as for every app.

## Checklist (master spec §48)

- **Purpose:** the user's files, without the shell.
- **Architecture:** `user/apps/files` on `oceans-ui` and `oceans-fs-proto`.
- **API:** none new.
- **Dependencies:** none.
- **Security:**
  - only `/home`, through the `files` permission;
  - deleting asks first.
- **Testing:** smoke: Files installed with the image, started, and its
  window (the toolbar) on screen.
- **Failure behaviour:**
  - a folder or file that cannot be read says so;
  - without `files`, Files says it may not open them.
