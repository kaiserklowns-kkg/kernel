# ADR-0046: Packages (.opk) and publisher signatures

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0025 (archive format), ADR-0045 (Oceans Core)

## Context

Apps arrive as files: downloaded, copied from a stick, and later from the
Store. The system must know:
- what a package is and who published it;
- that it was not changed since;
- what it will ask for, before any of it runs.

The master spec requires signed applications, permissions declared in
metadata, and a package format that can later grow dependencies, several
architectures and rollback (§22, §27, §28, §41). It also says not to
over-engineer the first version.

## Decision

### Format

A package is an **Oceans archive** (ADR-0025: validated table, CRC-32C
per file) holding:

| File | Holds |
|---|---|
| `manifest` | text, `key = value` lines (below) |
| the program | named by the manifest's `entry` |
| other files | anything the app needs |
| `signature` | **last**: `OCEANSIG`, the 32-byte Ed25519 public key, a 64-byte signature |

- **What is signed:** a SHA-512 digest over a context string, then for
  every other file in archive order: `[name length u32][name][size
  u64][SHA-512 of contents]`. Where files lie in the archive does not
  matter; their names, order, sizes and contents all do.

### Manifest

| Key | Required | Rules |
|---|---|---|
| `id` | yes | reverse-DNS, 2–8 labels of `a-z0-9-` starting with a letter, ≤ 64 bytes (it names a directory) |
| `name` | yes | ≤ 40 bytes of text |
| `version` | yes | `MAJOR.MINOR.PATCH`, ordered numerically |
| `publisher` | yes | must match the signing key's publisher |
| `architecture` | yes | `x86_64` today |
| `api` | yes | the API level needed (ADR-0045) |
| `entry` | yes | the program's file |
| `description` | no | ≤ 200 bytes |
| `channel` | no | update channel, `stable` by default |
| `source` | no | a URL |
| `permission` | repeatable | `NAME[: reason]`, names from the catalog (ADR-0047), ≤ 16, no repeats |

Unknown keys, repeated keys, control characters and malformed values are
refused, not ignored. A manifest means exactly what it says.

### Verification (`oceans-package`, no allocation)

`Package::open` checks, in order, and interprets nothing unverified:
1. the archive (structure, checksums);
2. the signature: present, last, well formed;
3. the signature against the digest (`verify_strict`);
4. the key is in the trust list;
5. the manifest parses;
6. its publisher is the key's;
7. its architecture is this system's;
8. its API level is offered;
9. its entry exists.

Each failure has its own message, so the user learns why.

### Trust

- **The trust list** is a text file, `KEY-HEX PUBLISHER` per line, in the
  **boot image** (`trust.keys`, given to Core as a module): the root of
  trust comes with the system.
- **A key belongs to one publisher name.** A trusted key cannot sign for
  another publisher.
- **Updates must be signed by the installed version's key** (ADR-0045):
  a different trusted key cannot take over an installed app.
- **The development key** (`tools/keys/oceans-dev.seed`) is public. It
  signs the examples, and development images trust it as `Oceans
  Examples`. Release images must not trust it; release key handling is
  part of the release process (Phase 10).

### Tooling

`oceans_package::build` writes a signed package, and `public_key_hex`
writes a trust line. `cargo xtask` builds the example's packages into
`build/packages/` with every image:
- Hello 1.0.0 and 2.0.0;
- for the tests, one signed by an untrusted key and one changed after
  signing.

## Consequences

- An app can be installed from anywhere: what matters is the signature,
  not where the file came from.
- **Verifying costs** one SHA-512 pass over the package, at install and
  at every start (ADR-0045).
- **Not in this version:**
  - several programs or architectures per package;
  - dependencies;
  - delta updates;
  - key rotation and revocation (a compromised key is removed from the
    trust list with a system update);
  - countersignatures (e.g. a Store's review).

  The format has room for them: more files, more manifest keys (unknown
  keys are refused today, so a new key is an explicit version change).

## Alternatives considered

- **Signing the whole archive bytes:** ties the signature to the layout,
  so a harmless rewrite (alignment, order of the table) would break it.
  Signing names, sizes and digests is stable.
- **X.509 certificates:**
  - far more parsing and policy than needed;
  - a publisher key list in the image is simple and auditable;
  - a certificate chain (e.g. for the Store) can be added as another
    signature kind later.
- **An existing format (tar, zip, a Linux package format):** Oceans
  already has a validated, allocation-free archive reader that the
  kernel and init use. Reusing it keeps one parser to trust.

## Checklist (master spec §48)

- **Purpose:** signed, self-describing app packages.
- **Architecture:** `libs/package` (format, manifest, catalog, verify,
  build) on `libs/archive`.
- **API:**
  - the package format;
  - the manifest keys;
  - `Package::open`, `trusted_keys`, `build`.
- **Dependencies:** ed25519-dalek and sha2 (RustCrypto, already used).
- **Security:**
  - strict signature verification before anything is interpreted;
  - publisher binding;
  - key continuity on update;
  - a trust root in the boot image;
  - strict manifest parsing.
- **Testing (host):**
  - manifests that parse, and every kind of bad one;
  - version ordering;
  - id rules;
  - trust list parsing;
  - signed packages open;
  - packages refused: a changed file with the old signature, a flipped
    byte, unsigned, an untrusted key, a key of another publisher, the
    wrong architecture, a newer API level, a missing entry;
  - the digest's sensitivity to names, sizes, contents and order.
- **Failure behaviour:** every refusal names its reason. Nothing from a
  refused package is used.
