# ADR-0081: Settings, and the rights of the system's own apps

- Status: Accepted
- Date: 2026-10-06
- Depends on: ADR-0047 (permissions), ADR-0048 (narrower Core
  capabilities), ADR-0080 (apps that come with the system), ADR-0072
  (release keys)
- Part of Phase 10 (Alpha: basic apps).

## Context

Settings has to show the apps and change their permissions. An app's
permissions (ADR-0047) reach the app itself, read-only information and
the network, never other apps. Only the shell (`app grant`) and the
desktop's dialogs could decide permissions.

Settings is an app (ADR-0080), so it needs a way to hold that authority
without every app being able to ask for it.

## Decision

### `manage-apps`, a permission only the system's own apps may hold

- **What it gives:** a Core end that may query, decide, manage and audit
  (ADR-0048's rights). It is minted for the app when it starts, like the
  shell's `core:` grants.
- **Who may ask:** only the system's own apps. An app is the system's own
  when both hold:
  - the image brings it (one of Core's `module:NAME.opk` grants, ADR-0080);
  - it is signed by a key that came with the image (the first entries of
    the trust list: the development key, or the release key, ADR-0072).

  Both are needed: in a development image the example apps share the
  development key without being part of the system, and a package that
  borrows a bundled app's id is not signed by the image's key.
  - Core refuses to install any other package that asks for it ("…is only
    for the system's own apps"), even from a developer the user trusts.
  - Core gives it only to a system app when it starts.

### The system's own apps are not asked

A system app is as trusted as the image it came with. Its permissions are
granted as automatic: no prompt, no dialog. The audit log still records
its installs and starts.

The user can still remove it (Settings can remove itself). A system update
brings it back.

### Settings

An app on the toolkit (ADR-0080) that comes with the system. Its
permissions are `window`, `system-info` and `manage-apps`.

- **General:** the release and architecture, the system call ABI, memory,
  how long the system has been up, the date and time.
- **Apps:**
  - the installed apps, with those running marked;
  - for the one chosen: its version, publisher, kind, state, key and id;
  - each permission with its decision, and Allow, Deny or Ask (ask again
    at the next start) for those not automatic;
  - "Remove this app".
- **Decisions made in Settings** are audited as made "in Settings" (a new
  `source::SETTINGS`).

## Consequences

- The system has a Settings app, and apps cannot reach other apps unless
  the system itself signed them.
- **The image's key carries more weight:** anything it signs may manage
  apps. A leaked release key was already the whole system (ADR-0072).
- **Still to come in Settings:**
  - the network (addresses, DNS);
  - sound;
  - the AI model;
  - trusted developer keys;
  - storage.
  Each needs a grant of its own, decided the same way.
- `manage-apps` appears in the permission catalogue, in the bridge's
  descriptions too (its test keeps them in step).

## Alternatives considered

- **Settings inside the display service:** a crash would take the desktop
  with it, and it would hold the desktop's whole authority.
- **Asking the user to allow `manage-apps`:** a third-party app could then
  ask for control over every app with one dialog; refusing it at install
  is simpler and safer.
- **A separate list of "system app" ids:** the key already says who
  published the app, and cannot be faked.

## Checklist (master spec §48)

- **Purpose:** Settings, and a safe way to give it its authority.
- **Architecture:**
  - `Permission::ManageApps`, `Permission::system_only`;
  - in Core: the install check, automatic decisions for system apps, and
    the minted end at start;
  - `user/apps/settings`.
- **API:**
  - the `manage-apps` permission;
  - `source::SETTINGS`.
- **Dependencies:** none.
- **Security:**
  - only image-signed apps may ask for it;
  - the end is limited to query, decide, manage and audit;
  - every decision is audited.
- **Testing:**
  - unit: the permission catalogue and the bridge's list in step;
  - smoke:
    - Settings installed with the image and its window on screen;
    - a trusted developer's package asking for `manage-apps` refused.
- **Failure behaviour:**
  - without the grant, Settings says so and shows no apps;
  - a refused decision shows why.
