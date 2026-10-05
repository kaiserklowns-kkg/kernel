# ADR-0072: Release keys and release images

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0046 (signed packages), ADR-0063 (developer keys),
  ADR-0068 (the USB image), ADR-0071 (system updates)
- Part of Phase 10 (Alpha: the first release).

## Context

Every image trusted the development key (tools/keys), for apps
(`trust.keys`) and for system updates (`update.keys`). Its seed is in the
repository, so anyone can sign an app or a whole system that such an image
accepts. That is right for development and wrong for anything published.
The 0.1.0 alpha needs an image that only its publisher can update.

## Decision

### The release key

- **What it is:** an `oceans keygen` key file (ADR-0063), for instance
  `oceans keygen "Oceans" --out ~/keys/oceans-release.key`.
- **Where it lives:** outside the repository, with whoever publishes
  releases.
- **How the build reads it:** `cargo xtask release` reads it from
  `OCEANS_RELEASE_KEY`.
- **What `release` refuses:**
  - a key file inside the repository, where it could be committed;
  - the development key's seed;
  - the development key's publisher name.

### Release images trust only the release key

- **The same key for both lists.** A release image's `trust.keys` and
  `update.keys` hold the release key and nothing else. The publisher of
  the system also publishes its first-party apps.
- **Other developers' apps** are trusted at the console, one key at a time
  (`app trust add`, ADR-0063/0067), as before.
- **Development images** keep the development key: QEMU, the smoke tests,
  and `cargo xtask usb`. `usb` now says, every time, that its image trusts
  a public key.

### `cargo xtask release`

`cargo xtask release` builds from a clean working tree only (a release is a
commit), in the release profile, the hardware layout (ADR-0068/0071). It
writes `build/release`:

```text
oceans-0.1.0-alpha-usb.img     the USB image: trusts only the release key
oceans-0.1.0-alpha.opk         the same release as a signed system update
release.keys                   the release key's public half
BUILD-INFO                     release, commit, key
SHA256SUMS
```

- **Publishing:** the release (a tag, a GitHub release with these files) is
  a separate, deliberate step by the publisher. The build never publishes.

### The hardware smoke test boots a release image

- **The test key:** `smoke-hw` uses a release image's keys with a test
  release key ("Oceans Test Release", public like the development key).
  The test's image therefore trusts a key other than the development key.
- **What it checks:**
  - an update signed with the development key is refused;
  - an update signed with the test release key installs and starts.

## Consequences

- **A published image accepts apps and updates only from its publisher**
  (and the developers its user trusts).
- **The release key is the system's root of trust.**
  - Losing it: no more updates for that release line. Users must rewrite
    their sticks with a release signed by a new key.
  - Leaking it: anyone can sign updates.
  - Keep a backup offline. There is no rotation or revocation yet; a
    signed key change (an update that brings a new `update.keys`) is later
    work.
- **Not covered:**
  - **the image itself:** a downloaded image is checked only by its
    SHA-256 from the same page. Signing `SHA256SUMS` is later work.
  - **Secure Boot:** it is not supported (UEFI keys are a separate,
    later decision).

## Alternatives considered

- **Different keys for apps and updates:** more to keep safe. A single
  publisher signs both today. The two lists stay separate files, so they
  can diverge later without a format change.
- **Keeping the key in CI secrets:** a release would be one push away for
  anyone with access to the repository settings. A key on the
  publisher's machine keeps releasing a human act.
- **Leaving development images unchanged and documenting the risk:** a
  test that boots an image trusting only another key is what proves the
  development key is not built in somewhere else.

## Checklist (master spec §48)

- **Purpose:** published images only their publisher can update.
- **Architecture:**
  - `ImageKeys` (development or release) feeds `trust.keys` and
    `update.keys`;
  - `cargo xtask release`;
  - `smoke-hw` on a release-keyed image.
- **API:** `cargo xtask release`; `OCEANS_RELEASE_KEY`; the release files.
- **Dependencies:** none new; the xtask uses the `oceans` tool's key file
  format.
- **Security:**
  - the private key never enters the repository, the image or CI;
  - keys that cannot be private are refused;
  - releases come from clean commits.
- **Testing:**
  - unit tests: release lists hold only the release key; private-key
    checks; SHA-256;
  - smoke-hw: development-signed update refused, release-signed one
    installed and booted.
- **Failure behaviour:**
  - no key, a bad key or a dirty tree: `release` stops, nothing is
    written;
  - `build/release` is rebuilt whole each time.
