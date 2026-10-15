# ADR-0159: Removing an app leaves nothing; resetting one starts it as new

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0045 (the runtime's apps), ADR-0047 (storage), ADR-0081
  (Settings), ADR-0109 (apps kept in the dock), ADR-0115 (each account's
  dock), ADR-0146 (the promises)
- Part of Phase 10 (Alpha). No ABI change.
- Plan: WP-8 of [development/plan-lessons-from-other-systems.md](../development/plan-lessons-from-other-systems.md).

## Context

Promise P3 (ADR-0146): the machine is the user's.

- **Windows'** uninstallers leave folders, registry keys and services
  behind. Years of that is why people reinstall Windows.
- **macOS** leaves `~/Library` folders behind when an app is dragged to the
  bin.
- **Neither** offers a plain "start this app over" for an app that has got
  into a bad state.

Oceans already kept each app's program and data under `/apps/ID`, and
removing an app (ADR-0045) deleted that folder and the app's decisions,
enabling and pending restarts. One trace stayed: a line in the dock file
of every account that kept the app in Wave. It was hidden while the app was
gone, so installing the app again brought it back into Wave unasked.

There was no way to reset an app short of removing and reinstalling it.

## Decision

### Removing leaves nothing

`REMOVE` (without "keep my data") also takes the app's line out of every
account's dock file: the administrator's `dock`, and each other account's
`dock.LOGIN` (ADR-0116). Together with what it already did, nothing of the
app is left: no program, data, decisions, usage times (ADR-0156), enabling,
pending restarts or dock lines.

The audit records the removal, as before.

### Reset: `RESET_APP` (Core op 53, `MANAGE`)

data = the app's id.

1. The app is stopped if it runs.
2. Its data (`/apps/ID/data`, the `storage` permission's folder) is
   erased.
3. The next start makes the folder again, empty, so the app starts as on
   its first day.

- Its permissions stay as the user decided them. A reset is about the
  app's own state, not about trust.
- Audited: `reset ID: its data erased[; it was stopped]`.
- It needs `MANAGE`, as `REMOVE` does. Settings has it through
  `manage-apps`, and the shell has it.

### Settings → Apps

- A **Reset** button sits beside **Remove this app**, with a line saying
  what Reset does.
- **Each asks for a second press,** its label naming what it erases
  ("Press again: erase its data", "Press again: remove it and its data").
- A first press holds only for the app it was made on, and changing a
  permission disarms it.

### The shell

`app clear ID` erases the app's data. (`app reset ID PERMISSION`, which
asks for a permission again, keeps its meaning.)

## What the user sees change

- **Settings → Apps:** a Reset button. Reset and Remove each take two
  presses.
- **Wave:** an app removed and installed again is no longer kept in Wave.

## Consequences

- An app that keeps a bad state can be started over without losing its
  permissions or reinstalling it.
- "Always open with" (ADR-0099) is still not built, so there is no default
  to clean up. When it is, removal must clean it too: this ADR is the
  rule.
- Notifications already shown at the bell (ADR-0119) stay until cleared:
  they are the user's record of what happened.

## Alternatives considered

- **Reset also forgets the permissions.** It lost: people reset an app
  because it misbehaves, and asking every permission again would punish
  them for that. "Ask" in Settings (ADR-0047) already does that one at a
  time.
- **A confirmation dialog** instead of a second press. It lost: the
  toolkit has no modal dialog yet, and a button whose label says exactly
  what the next press erases is as clear and stays in place.
- **Leave dock lines for apps that come back.** It lost on P3: a removed
  app should not return to Wave unasked.

## Checklist (master spec §48)

- **Purpose:** an uninstall that leaves nothing, and a way to start an app
  over.
- **Architecture:**
  - Core's `remove` cleans every dock file;
  - Core's new `reset_app`;
  - Settings' two buttons, armed by a first press;
  - the shell's `app clear`.
- **API:** Core op 53 `RESET_APP` (`MANAGE`). No ABI change.
- **Dependencies:** none.
- **Security:**
  - reset and remove need `MANAGE`;
  - a reset never touches the app's program, its permissions or another
    app;
  - both are audited.
- **Testing:**
  - smoke, boot 2: `hello` has counted 6 to 11 runs in its storage;
  - `app clear` erases them (audited), and its next run is "run 1";
  - then it is removed, as before.
- **Failure behaviour:**
  - an erase that fails stops at the first error and reports it (the app
    stays stopped);
  - an unreadable dock file is left as it is;
  - a missing data folder is not an error.
- **Promises touched:** P3 (kept), P7 (one button added, two now ask
  twice).
