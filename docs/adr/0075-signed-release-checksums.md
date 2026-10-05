# ADR-0075: Signed release checksums and the published release key

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0072 (release keys)
- Part of Phase 10 (Alpha: the first release).

## Context

A release image is downloaded from the same page as its `SHA256SUMS`
(ADR-0072). Whoever could change the image there could change the
checksums too. The image itself trusts only the release key, but nothing
let a user check the image *before* writing it to a stick and booting it.

## Decision

### The checksums are signed

- **What `cargo xtask release` adds:** it signs `SHA256SUMS` with the
  release key into `SHA256SUMS.sig` (Ed25519, in hex).
  - What is signed is a fixed context line, then the text. A release
    signature can therefore never pass for a package's, or the reverse.
- **It checks its own output:** `release` verifies what it wrote before it
  finishes.

### `oceans verify`

```text
oceans verify DIR --key KEY-HEX
```

`oceans verify` checks the signature of `SHA256SUMS` against `KEY-HEX`,
then the SHA-256 of every file it lists.

- **The key is required,** and is never taken from the download.
- **The list can name only plain file names** beside it: no `/`, `\`, `:`
  or leading `.`. A list can never point the check elsewhere on the disk.

### The release key's public half is in the repository

- **The file:** `tools/keys/oceans-release.pub` holds the key users check
  against. Its history is the repository's, so a change to it is visible
  and reviewed.
- **The first release key:** `5fcfd97a…c2992f87f0`, publisher `Oceans`.
- **`cargo xtask release` refuses any other key.** A release is never
  signed with a key users cannot check it against.

## Consequences

- A user can check a download offline, with the `oceans` tool and the key
  from the repository, before trusting it with a machine.
- **Limits:**
  - **No rotation yet.** Changing the release key means a new
    `oceans-release.pub` and new images. Systems already installed keep
    trusting the old key for updates (ADR-0072).
  - **Checking needs the `oceans` tool** (Rust, from this repository).
    `SHA256SUMS` still works with `sha256sum` alone, without the
    signature's assurance.
  - **Secure Boot** remains out of scope.

## Alternatives considered

- **GPG or minisign signatures:** a second key type and tool for the
  publisher to keep. The release key and Ed25519 are already there, and
  the check is a few lines in a tool the project ships.
- **Signing the image itself:** a raw disk image has no place for a
  signature. Signing the list covers every file at once.

## Checklist (master spec §48)

- **Purpose:** check a download before booting it.
- **Architecture:**
  - `oceans_dev::release` (sign, verify);
  - `cargo xtask release` writing `SHA256SUMS.sig`;
  - `tools/keys/oceans-release.pub`.
- **API:**
  - `oceans verify DIR --key KEY-HEX`;
  - `SHA256SUMS.sig`.
- **Dependencies:** `ed25519-dalek` for the `oceans` tool. It is already
  used, through `oceans-package`.
- **Security:**
  - domain-separated signatures;
  - the key comes from outside the download;
  - names are confined to the folder;
  - releases are signed only with the published key.
- **Testing:** unit tests:
  - a signed release verifies;
  - a changed file, a re-hashed list, another key or a bad key text fails;
  - names reaching outside the folder are refused;
  - the published key is not a test key, and the publisher name must
    match.
- **Failure behaviour:** every mismatch names the file and the reason, and
  nothing is trusted partially.
