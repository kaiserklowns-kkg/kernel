# ADR-0038: Rename

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0019 and ADR-0022 (filesystem), ADR-0035 (mounts),
  ADR-0037 (FAT writes)

## Context

The file protocol could create, write and remove, but not rename. A file
could only be moved by copying and deleting it, which is slow and not
atomic: a crash in between leaves both copies, or neither. Renaming is
also what makes "write a new version, then replace the old one" safe, the
usual way programs update files.

It has to work on every filesystem the system serves:

- the Oceans volume (copy-on-write, committed atomically);
- FAT (no journal, ordered writes, ADR-0037);
- through mount points (ADR-0035).

## Decision

- **Protocol (`RENAME`, op 12):**
  - Sent on a directory handle opened for writing.
  - `data = [old length u8][old path][new path]`. Both paths are relative
    to that directory and may contain `/`.
  - Any move within the handle's subtree is possible, but nothing outside
    it: the capability rule (write access comes through a writable
    directory) holds unchanged.
  - **Semantics:**
    - an existing file at the new path is replaced; an existing empty
      directory too, by a directory;
    - renaming onto itself does nothing;
    - moving a directory into itself or below itself is refused;
    - open handles keep working (their nodes do not change).
  - **New status:** `CrossDevice` ("not on the same filesystem").
  - **Client and shell:** the client call is `Node::rename(old, new)`, and
    the shell gains `mv OLD NEW`.
- **The Oceans volume (`Volume::rename`)** changes only the in-memory
  tree: the entry moves, a replaced node is unlinked (and freed once
  unused), and the depths of a moved subtree follow it.
  - It is refused if:
    - the result would be deeper than the volume allows;
    - read-only nodes are involved;
    - volatile nodes (`/bin`) would mix with nodes on disk.
  - The fs service commits right after, so the rename reaches the disk
    atomically, with all its parts, or not at all.
- **FAT (`Fat::rename`), ordered with barriers:**
  1. A replaced target's entry is deleted.
  2. The new entry is written (long names, then the short entry), with
     the same clusters, size, attributes and times.
  3. A moved directory's `..` is pointed at its new parent.
  4. The old entry is deleted.
  5. The replaced target's clusters are freed (or freed when its last
     handle goes).
  - A change of case only, in a short name, rewrites that one entry in
    place.
  - Open nodes follow their entry to its new place.
  - **Crashes and repair.** A crash between steps 2 and 4 leaves two
    entries for the same clusters. The repair after an unclean session
    (ADR-0037) now recognises this:
    - **Duplicates:** an entry whose first cluster another entry with the
      same size and kind already names is deleted. The first entry found
      is kept, so the rename either happened or did not, and no data is
      lost.
    - **`..` links:** every directory's `..` is checked against its
      actual parent and corrected.
    - Both are reported by `check` (`duplicates`, `parents`).
  - **Not atomic on FAT:** replacing an existing file. FAT cannot swap
    two entries, so the target's entry goes first, and a crash right
    after leaves the target gone and the source under its old name.
- **Mount points:**
  - In the system fs, a rename whose two paths are inside the same
    mounted filesystem is passed to that service with the mount prefix
    removed.
  - One between filesystems is refused with `CrossDevice`.
  - Mount points themselves cannot be renamed.
  - Below a mount point, `RENAME` on a forwarded handle is forwarded like
    any other change.

## Consequences

- Files can be renamed and moved on every volume. On the Oceans volume
  this includes atomic replacement, so "write elsewhere, then rename"
  updates are crash-safe there.
- **Moving across filesystems** is not done by `mv`: it reports
  `CrossDevice`, and copying is up to the caller (a later `cp` utility).
- **Size limit:** paths are limited by the request size (248 bytes for
  both together).

## Alternatives considered

- **A target directory as a capability (`RENAME` carrying a second
  handle):** the most general form. But the server cannot yet tell which
  of its own badged handles it received, and paths below one writable
  directory cover what callers need.
- **Rename only within one directory:** then moves would need copies,
  and the FAT and Oceans code would be no simpler.
- **Making FAT's replace atomic with an intermediate name:** this needs
  an extra entry and still leaves a crash window. The documented order
  loses nothing and is repaired to a valid state.

## Checklist (master spec §48)

- **Purpose:** renaming and moving files and directories.
- **Architecture:** `RENAME` in fs-proto, `Volume::rename`,
  `Fat::rename`, and forwarding in the system fs.
- **API:**
  - op 12 and status 14;
  - `Node::rename`;
  - `mv` in the shell;
  - `Report::{duplicates, parents}`, `Recovery::{duplicates, parents}`.
- **Dependencies:** none.
- **Security:**
  - write access to the directory handle is required;
  - paths are validated component by component and confined to the
    handle's subtree;
  - renames never cross filesystems or move mount points.
- **Testing:**
  - **Oceans volume:**
    - renames within and across directories;
    - replacing a file that stays readable through its handle;
    - moving a directory, with its depth updated;
    - refusals: into itself, file over directory, invalid names, depth,
      volatility;
    - the result surviving a remount.
  - **FAT:**
    - the write scenario now also renames (a long name, a change of case,
      a move, a replace while open, a directory move);
    - the crash test covers every barrier of those renames;
    - after repair, no duplicates, no wrong `..`, nothing lost;
    - `fsck.fat` and mtools accept the results.
  - **Smoke:**
    - a move between directories on the system disk;
    - a rename on the Oceans stick through `/usb`;
    - a refused cross-filesystem move;
    - on FAT, a directory move and a file moved into it, checked on the
      host with oceans-fat and `fsck.fat`.
- **Failure behaviour:**
  - refused renames change nothing;
  - on FAT, a crash mid-rename is repaired to one consistent outcome.
