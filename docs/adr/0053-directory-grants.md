# ADR-0053: Directory grants from init, and the AI's settings

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0016 (init), ADR-0019 (file protocol), ADR-0050 (Go on
  Oceans), ADR-0051 (the AI runtime)

## Context

A boot-image service that needs to keep a few files had two options.
**`use = fs`** gives the root of the whole filesystem, everyone's files:
far beyond least privilege. **Nothing** means its state is gone at every
restart.

The AI service hit this first: `ai model URL MODEL` was lost when the
service restarted. The file protocol already makes a directory handle a
capability for that directory and everything below it, and nothing else
(no `..`, ADR-0019). Only init could not hand one out.

## Decision

- **`grant = storage:/PATH`**: init opens directory `PATH` through the
  `fs` endpoint, creating missing directories, and hands the service the
  handle, writable, as **`use storage`**.
  - The path is absolute, with plain names only (no `.`, `..`, empty
    parts), checked when `services.conf` is read.
  - The service can create, read and change files there, and nothing
    outside it.
  - The grant needs `fs` started earlier in `services.conf`. Without it
    (or if a path component is a file), the service is not started, and
    init logs why.
- **The AI service** gets `grant = storage:/system/ai`:
  - `CONFIGURE` writes the model setting to `model.conf` there, durably;
  - at start the service applies the saved setting ("model settings
    restored"). A setting that does not parse is ignored, with the
    service starting unconfigured.
- **The Go file binding** (`go/oceans/fs`) gives Go programs the file
  protocol:
  - open, walk, stat, read, write, truncate, sync, list, remove;
  - `WriteFile`, `ReadAll`;
  - inline requests (248 bytes each), since Go modules have no shared
    buffers (ADR-0050). It is fine for settings and text files.

## Consequences

- Services keep state with exactly the reach they need. Any service can
  use it: give it `storage:/system/NAME`.
- The AI's model setting survives restarts and reboots.
- **The user owns `/system`:** the shell holds the filesystem root, and
  can read or change these files (they are the user's machine's
  settings); services cannot reach each other's directories.
- **Not yet:** read-only directory grants (`storage-ro:`), and quotas per
  directory.

## Alternatives considered

- **`use = fs` for services that store anything:** every such service
  could read and change all files.
- **Settings stored by the shell and replayed at start:** they would
  depend on a shell being there, and the service could not keep anything
  else.
- **A settings service:** another protocol and process, for what a
  directory capability already gives.

## Checklist (master spec §48)

- **Purpose:** least-privilege persistent storage for boot-image
  services; persistent AI settings.
- **Architecture:**
  - init's `storage:` grant (fs-proto from init);
  - `go/oceans/fs`;
  - the AI service's settings file.
- **API:**
  - `grant = storage:/PATH` → `use storage`;
  - the Go `fs` package.
- **Dependencies:** none.
- **Security:**
  - the handle reaches only that directory's subtree;
  - paths are validated;
  - no service gets the root for state.
- **Testing:**
  - **Go:** unit tests elsewhere use the binding's types; the smoke test
    covers it end to end.
  - **Smoke:**
    - the AI's setting is stored at boot 1 and restored at boot 2;
    - the host finds `/system/ai/model.conf` on the disk.
- **Failure behaviour:**
  - a bad path refuses `services.conf` with a message;
  - an unavailable filesystem fails the service's start, and init logs
    it;
  - a setting that cannot be saved is reported to the requester ("set,
    but not saved").
