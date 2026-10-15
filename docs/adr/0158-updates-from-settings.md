# ADR-0158: Updates from Settings, restarted when the user says

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0071 and ADR-0092 (updates and their slots), ADR-0075
  (signed release checksums), ADR-0122 (`update previous`), ADR-0128 (how
  Oceans Setup gets a release), ADR-0135 (`oceans-web`), ADR-0146 (the
  promises)
- Part of Phase 10 (Alpha). No ABI change.
- Plan: WP-4 of [development/plan-lessons-from-other-systems.md](../development/plan-lessons-from-other-systems.md).

## Context

Promises P3 and P6 (ADR-0146): the machine is the user's, and a failure
can be undone.
- **Windows** downloads and restarts on its own schedule.
- **macOS** nags until you agree.

Oceans had the safe half: a signed update goes into the slot not in use,
and the previous release stays in the boot menu (ADR-0071). But updating
meant fetching a file and typing `run update out use:fs -- apply PATH` in
the shell. No ordinary user could do that.

There was also a quieter gap: **the system's own apps did not follow an
update.** Core installs an app from the image only when it is *newer*
(ADR-0080), and Settings had stayed at version 1.5.0 since ADR-0115, through
many changes. A machine updated to a new release kept its old Settings.

## Decision

### `oceans-update`: one implementation of updating

The `update` command's work (status, apply, previous, Secure Boot's
partitions) moves out of the program into the `oceans-update` crate
(`user/update-core`). Progress is said through a `Report` trait: the
console for the command, and a kept line for the service. The command is
now a thin front. Its behaviour and its words are unchanged, and the
hardware smoke test still checks them.

### The `updater` service

A service started by init, with only what updating needs: `log`, `provide
= updater`, `use = net`, `use = fs` (the root: `/bin`, `/system` and the boot
partition) and `use = usbdisk` (for Secure Boot's partitions, ADR-0092).
It has no Core end and no window.

- **The source** is a URL ending in `/` that holds a release as `oceans
  release` publishes it (ADR-0075): `SHA256SUMS`, `SHA256SUMS.sig`, and the
  system package.
  - The default is the published releases' latest
    (`https://github.com/kaiserklowns-kkg/kernel/releases/latest/download/`),
    which Oceans Setup also uses (ADR-0128).
  - `/system/updates/source` may name a mirror. The hardware smoke test
    names its host's test release this way.
- **Trust,** checked in order, each before anything is written to a slot:
  1. `SHA256SUMS` is signed by a key in `/bin/update.keys`
     (`oceans_package::sums`, host-tested; the same format `oceans release`
     signs, which a test in `tools/oceans` checks);
  2. the downloaded package matches its checksum (SHA-256, computed as it
     arrives);
  3. `oceans-update` verifies the package's own signature by an update
     key, its id `system.oceans`, and that it is newer than the running
     release (ADR-0071).

  HTTPS adds privacy, not trust: a mirror over plain HTTP is safe.
- **Its states:** idle, checking, up to date, available, downloading,
  installing, installed (it starts when the user restarts), failed. A
  failure says why, and leaves both slots as they were.
- **It never restarts the machine.** An installed update waits for the
  user's restart. The previous release stays in the boot menu, and "Use
  the previous version" (`update previous`) chooses it for the next boot.
- **By itself it only looks.** With "check automatically" on (the default,
  kept in `/system/updates/auto`), it looks once a day, the first time ten
  minutes after the start. It never downloads or installs by itself. This
  is the second connection Oceans makes unasked, beside the network time
  (ADR-0146, P1), and it can be turned off. The smoke test's configuration
  starts it off (`args = check-when-asked`): its first boot runs longer
  than ten minutes, and a test must not reach the internet. The test
  checks the service says so.
- **It stays responsive.** Every request is answered at once, and a
  download goes on between requests, 64 KiB per 20 ms tick
  (`oceans_web::Response::read` without waiting). Only the connection and
  the TLS handshake block, for seconds at most.

### The protocol

`oceans_core_proto::updater`:

| Op | Data | Does |
|---|---|---|
| `STATUS` (1) | — | → `[state][auto][percent]` and a line of text |
| `CHECK` (2) | — | looks at the source |
| `INSTALL` (3) | — | downloads, checks and installs what was found |
| `PREVIOUS` (4) | — | the previous release starts first from the next boot |
| `AUTO` (5) | `[0 \| 1]` | look by itself once a day, or only when asked |
| `SOURCE` (6) | a URL ending in `/`, or empty | a mirror, or the published releases again |

Replies are `OK`, `BUSY` (a download or an install is under way),
`BAD_REQUEST` or `NOT_NOW`.

### Who may ask: `system-update`

- A new permission, **`system-update`**, is for the system's own apps
  only (like `manage-apps` and `system-settings`, ADR-0081).
- Core holds an updater end (`use = updater`) and gives Settings a
  duplicate.
- Core does **not** relay the calls itself: a check's connection would
  stall Core, which every app depends on.
- The shell gets its own end (`use = updater`), for `updates`.
- The permission catalog is mirrored in the Go bridge
  (`go/cmd/bridge/system.go`), and a test keeps the two equal.

### Settings → General → Software update

- **What it shows:** what is running and what is in the other slot, what
  is available, or how far a download is (Settings now wakes once a
  second, so the progress and the clock move).
- **Its buttons:** Check now; Download and install (when something was
  found); Use the previous version; Check automatically (filled while on).
- **Once installed,** it says the update starts at a restart, from
  Launchpad. Settings has no button that restarts.

### The shell

`updates [check | install | previous | auto on|off | source URL]`.
- `install` follows the download and the install to their end.
- Neither the shell command nor the service restarts anything.

### The system's own apps follow the release

At start, Core compares each app the image brings with the installed one:
- the same bytes: left alone;
- the same version but different bytes (a new release changed it without
  a new version): installed over it, as a newer one would be;
- an older version: still refused.

Only apps from the image get this. An app from anyone else still needs a
newer version (ADR-0046).

## What the user sees change

- **Settings → General:** a "Software update" part, with its buttons.
- **The network:** Oceans looks for updates once a day, starting ten
  minutes after the start. It can be turned off.
- **After an update:** the system's own apps are the new release's.

## Consequences

- Updating is a button press, and restarting stays the user's choice.
- The ZBook can take 0.1.2 from Settings once it runs a release with this
  ADR. The first step still goes through the shell or Oceans Setup.
- A release must publish its package as `oceans-VERSION-CHANNEL.opk`. The
  version in the name decides whether to offer it, and the package itself
  decides whether to install it.
- With Secure Boot the service applies through the stick, as `update`
  does, because it holds `usbdisk`.
- Settings and the shell can stall for the seconds a check's connection
  takes. Making the connection non-blocking is left to a later ADR.

## Alternatives considered

- **Core relays the updater's operations (Core ops 53–56, first reserved
  for this).** It lost because a check would stall Core and every app with
  it. The reservation was released.
- **Settings runs the download itself.** It lost because an app would need
  the network, the boot partition and the update keys: far more than
  Settings should hold.
- **Spawn `/bin/update` from the service.** That needs a pipe for its
  words and a way to spawn programs from a service. Sharing the code as a
  crate is simpler and keeps one implementation.
- **Download in the background by itself (Windows' way).** It lost on P3:
  the user decides when bandwidth and disk go to an update.
- **Restart by itself at night.** It lost on P3.

## Checklist (master spec §48)

- **Purpose:** updates anyone can install, never taking the machine away.
- **Architecture:**
  - `oceans_package::sums`;
  - `oceans-update` (shared);
  - the `updater` service;
  - `oceans_core_proto::updater`;
  - the `system-update` permission;
  - Core's end and its image apps that follow the release;
  - Settings' Software update;
  - the shell's `updates`.
- **API:**
  - the updater's six operations;
  - the `system-update` permission (the catalog grows to 12; appended, so
    no number changes);
  - no kernel ABI and no Core ops.
- **Dependencies:** `sha2` in the updater (already in the tree); no new
  crates.
- **Security:**
  - signed checksums, the checksum, then the package's signature and
    version, before any write;
  - the service holds no Core end;
  - only system apps get `system-update`;
  - a source must be http(s) and end in `/`;
  - nothing restarts.
- **Testing:**
  - `oceans_package::sums`: signed lists read, changed or unsigned ones
    refused, names that reach elsewhere refused;
  - `tools/oceans`: what `oceans release` signs verifies on the device;
  - the bridge's catalog test;
  - `smoke-hw`: the installed SSD's first boot gets 0.1.1 from the host's
    test release through `updates check` and `updates install`, and the
    next boot runs it (the boot that follows already checks `0.1.1
    alpha`);
  - the smoke test checks Settings' permissions.
- **Failure behaviour:**
  - any failed step leaves both slots as they were and says why;
  - a partial download is removed (also at the next start);
  - a power cut during the install is ADR-0071's: one complete boot
    configuration is always there.
- **Promises touched:** P3 (kept), P6 (the previous version one button
  away), P1 (one daily look, which can be turned off), P7 (Settings
  gains a part).
