# ADR-0061: The Store, and the end of Phase 7

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0046 (packages and signatures), ADR-0047 (consent),
  ADR-0048 (narrower Core capabilities), ADR-0054 (http/https from Go),
  ADR-0057 (system dialogs), ADR-0058 (web experience and bridge)

## Context

Phase 7 asks for desktop, launcher, settings, system monitor, Store, AI
Center, notifications and permission dialogs (master spec §47). Everything
but the Store existed:
- **native:** desktop, launcher, notifications, permission dialogs, app
  windows (ADR-0057, ADR-0059, ADR-0060);
- **web:** Control Center (the system monitor), Apps, AI Center, Settings
  (ADR-0058).

Apps could only be installed from the console (`app install FILE`).

A Store adds an installer that is reachable from another device, so it
must not weaken anything:
- what gets installed is still decided by publisher signatures
  (ADR-0046);
- the decision to install is the user's, made on the device;
- a paired browser must not be able to install on its own.

## Decision

### Where apps come from

A **store** is a directory on a web server:
- **`index.json`** lists its apps: id, name, version, publisher,
  description, permissions, package file, size, SHA-256;
- the **packages** (`.opk`) sit beside it.

The catalog is **not trusted**. It only says what to fetch:
- the bridge checks every entry: ids, versions, plain file names (no
  paths), sizes up to 32 MiB, 64-hex-digit hashes;
- it checks the download against the listed size and SHA-256;
- what makes a package installable is still its signature by a trusted
  publisher key, checked by Oceans Core.

The Store's URL is the user's choice (Store page). It is kept in the
bridge's own storage.

### Install = propose, then confirm on the device (Core)

- **New access right `propose`.** Paired browsers now get it, besides
  `query`, `run` and `audit`.
- **`PROPOSE (length, [memory])`:**
  - the package is handed over as a memory object;
  - Core verifies it as `INSTALL` would (signature, trusted key, same key
    and newer than what is installed) and **keeps a copy**;
  - only one proposal waits at a time (`Pending` otherwise);
  - it is audited as "proposed installing … (waiting for the user on the
    device)".
- **`PENDING`** (needs `query`): the waiting proposal's id, version, name,
  publisher, permissions, the version it updates, and description.
- **`ACCEPT (number, install)`** (needs **`decide`**, which pairing never
  grants):
  - installs **exactly the bytes that were verified and shown**, so
    nothing can be swapped between the question and the answer;
  - or discards them;
  - audited as "confirmed / declined on the device".

### The dialog (display service)

The desktop polls `PENDING` every 2 s, as it polls the app list. It shows
a system-drawn dialog ("Install an app" / "Update an app") with:
- the verified identity: name, version, id, publisher;
- the permissions the app may ask for;
- its description.

The buttons are Cancel and Install.

### The bridge and the web page

- **API:** `GET /api/store` (the catalog, each app's state: available,
  installed or update), `POST /api/store/source`, `POST /api/store/install`
  (downloads, checks, proposes; answers 202 "confirm on the device").
- **The Store page:** the store's URL, its apps, and install/update
  buttons that say where to confirm.
- **Its new grants:** `storage:/system/bridge` and the system's root
  certificates, for https stores.

### Bulk downloads from Go

An interpreted Go program reading a 2.3 MB package one message at a time
(about 240 bytes) took 65 s. Two changes fix that:
- the Go host gains **`memory_create`**;
- Go TCP connections attach a **256 KiB shared buffer** (`TCP_ATTACH`,
  `TCP_RECV_BUF`, ADR-0030), which the model gateway's transport now does
  for every connection.

The same install takes about 15 s. The package also goes to Core as one
memory object, not as a file written in small pieces.

### Phase 7 is complete

**Exit criterion:** a user installs an app from the Store, confirms in a
system dialog, and uses its window. `cargo xtask smoke` covers every
step:
1. the browser sets the store;
2. Tiles is listed as available, and proposed;
3. the desktop's install dialog appears and Install is clicked;
4. Core installs Tiles;
5. Tiles opens a window, which answers a key.

**Later work, outside Phase 7:**
- **The on-device web view** (ADR-0056): the web apps are used from
  another device until Oceans has an HTML engine. This is a decision of
  its own.
- **Keys beyond bytes** (arrows, function keys) and resizing windows
  (ADR-0059).
- **TLS for the bridge itself** (ADR-0058).
- **Signed catalogs and several stores.** Signatures on packages already
  carry the trust.

## Consequences

- Apps can be found and installed without the console, and installing
  stays the user's decision on the device.
- A compromised browser or bridge can, at worst, put a question on the
  screen that the user declines. It cannot install, choose permissions, or
  swap what is installed.
- Proposals live in Core's memory: one at a time, at most 32 MiB.
- Smoke boot 1 now covers the whole Phase 7 path. The pointer numbers in
  later steps move by one, because a mouse is plugged in once more.

## Alternatives considered

- **Letting a paired browser install directly:** this makes the web layer
  part of the trust path, against ADR-0056.
- **Keeping only the proposal's file name or hash in Core:** the bytes
  could change between the question and the install. A copy is simple and
  exact.
- **A separate Store service:** more authority to manage, for logic the
  bridge, which serves the Store page, already has the means for.
- **Signing catalogs now:** packages are already signed, and the
  catalog's role is only discovery.

## Checklist (master spec §48)

- **Purpose:** the Store, the last item of Phase 7.
- **Architecture:**
  - store catalog over http(s) (`go/store`), fetched by the bridge;
  - Core proposals with on-device confirmation;
  - a native install dialog.
- **API:**
  - Core `PROPOSE`, `PENDING`, `ACCEPT`, `access::PROPOSE`,
    `Status::Pending`;
  - bridge `/api/store`, `/api/store/source`, `/api/store/install`;
  - gohost `memory_create`; Go `oceans.MemoryCreate`,
    `tcp.Conn.UseSharedBuffer`, `httpc.Get`, `Transport.Get`.
- **Dependencies:** xtask uses `sha2` (already in the workspace) to write
  the test store's index.
- **Security:**
  - signatures decide what can be installed, and the user decides what is;
  - the catalog is untrusted and checked;
  - downloads are checked against the catalog;
  - exactly the verified bytes are installed;
  - accepting needs `decide`, held by the user's agents (desktop, shell),
    never by pairing;
  - everything is audited.
- **Testing:**
  - unit: `go/store` (catalog bounds, hashes, states), bridge store routes
    (lists, only proposes, refuses mismatches, cross-site and unpaired
    requests), httpc `Get` and its limit, and the web client (catalog
    states, install by id only);
  - smoke: the full path above, with the dialog checked on screen.
- **Failure behaviour:**
  - no store set: an empty list;
  - an unreachable store: 502 with the reason;
  - a mismatched download: refused before Core sees it;
  - a package Core refuses: 422;
  - another install waiting: 409;
  - Cancel: the copy is discarded and the decline audited.
