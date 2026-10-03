# ADR-0039: Copying with shared buffers

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0019 (file protocol), ADR-0030 (shared buffers),
  ADR-0035 (mounts), ADR-0038 (rename)

## Context

Files could be renamed within one filesystem (ADR-0038), but nothing
copied them, and `mv` between filesystems (the system disk and a stick at
`/usb`, say) stopped with `CrossDevice`. Removing a directory also meant
removing everything in it by hand, one entry at a time.

A copy is mostly data movement. Through inline requests (248 bytes each)
a 1 MiB file takes thousands of calls. A shared buffer per handle
(ADR-0030) cuts that to a few, but a naive copy still moves every byte
twice through the copying program: into its memory from the source's
buffer, and out again into the destination's.

## Decision

- **One buffer, two handles.** A `Shared` buffer is now an object of its
  own (`Shared::new`). `Node::share` attaches it to another handle, by
  sending a duplicate of the same memory object with `ATTACH`.
  - With the buffer attached to both the source and the destination
    handle:
    - `read_buffer` has the source's service read into it;
    - `write_buffer` has the destination's service write from it.
  - The bytes never pass through the copying program, also when the two
    handles are served by different filesystems: below a mount point
    `ATTACH` is forwarded like any other request (ADR-0035), so the
    mounted service maps the very same pages.
  - The protocol is unchanged: `ATTACH`, `READ_BUF` and `WRITE_BUF` as in
    ADR-0030, now with `at = 0` from the client helpers.
- **`oceans_fs_proto::tree`:**
  - **`Copier`** holds the buffer and running totals (files, directories,
    bytes).
    - `file` truncates the destination, then copies until the source ends.
    - `directory` copies the entries of one directory into another:
      missing entries are created, files replaced, and directories merged
      into, down to `MAX_DEPTH` (32) levels.
    - Errors say which side failed (`Side::Source`, `Side::Destination`).
  - **`remove_tree`** removes an entry and, if it is a directory,
    everything in it, bottom-up.
  - Both need no allocator, so any program can use them.
- **The shell:**
  - **`cp [-r] FROM TO`:**
    - copies a file, or with `-r` a directory with all it holds;
    - when `TO` is an existing directory, the copy goes into it under its
      own name;
    - refused: copying a file onto itself, a directory into its own
      subtree, a directory without `-r`, and the root;
    - the destination is synced before the summary (`cp: N bytes in F
      files, T ms`) is printed: success means it is on disk.
    - Buffer size: 128 KiB.
  - **`mv` between filesystems:** when the rename answers `CrossDevice`,
    `mv` copies (recursively), syncs the copy, and only then removes the
    source (and syncs that filesystem too).
    - A failure anywhere before the removal leaves the source untouched.
    - As with a rename, the destination path is exact, and a directory
      may replace only an empty directory.
    - Mount points still cannot be moved: that refusal comes before
      `CrossDevice`.
  - **`rm -r PATH`:** removes a directory and everything in it. The root
    is refused. Mount points are refused by the fs, before anything below
    them is touched.

## Consequences

- Files and trees move freely between the system disk, Oceans sticks and
  FAT sticks, in both directions.
- **Speed:** in QEMU, 1 MiB copies from the system disk in about 0.3 s to
  an Oceans stick and about 0.6 s to FAT, each including the sync.
- **Not atomic:**
  - A crash during a copy leaves a partial destination; a crash during a
    cross-filesystem `mv` can leave both copies, but never neither.
  - A `cp` onto an existing file truncates it first: a failure part-way
    leaves it short. On the Oceans volume, "copy elsewhere, then `mv`"
    gives an atomic replacement.
- **Same-file checks are by path.** Two different paths reaching one file
  do not exist today (no links, and each mount is reachable through one
  path only).

## Alternatives considered

- **A copy operation in the protocol (`COPY` from one handle to
  another):**
  - it would save even the buffer round trip within one filesystem;
  - but between two services the data has to cross anyway, and a server
    cannot yet tell which of its own handles it received (ADR-0038).
  - The shared buffer gives the same zero-copy path for both cases with
    no protocol change.
- **Copying through the program's own memory (`read_shared` then
  `write_shared`):** simpler, but every byte is copied twice more for
  nothing.
- **A separate `cp` program in `/bin`:**
  - it would need `use:fs` granted on each run, unlike a built-in;
  - and `mv` needs the copy anyway.
  - The logic lives in fs-proto, so a standalone program is a thin
    wrapper whenever one is wanted.

## Checklist (master spec §48)

- **Purpose:** copying files and trees, moving them between filesystems,
  and removing trees.
- **Architecture:**
  - `Shared::new` and `Node::share`, `read_buffer`, `write_buffer` in
    fs-proto;
  - the `tree` module (`Copier`, `remove_tree`);
  - the shell commands `cp`, `rm -r`, and `mv`'s fallback.
- **API:**
  - `Shared::{new, size}`;
  - `Node::{share, read_buffer, write_buffer}`;
  - `tree::{Copier, Totals, CopyError, Side, remove_tree, MAX_DEPTH}`.
- **Dependencies:** none.
- **Security:**
  - copies need read access to the source and write access through the
    destination's parent, as any write does;
  - the shared buffer is a memory object the copying program created;
    each service maps it only while the handle lives.
- **Testing (smoke, checked on the host afterwards):**
  - **system disk → Oceans stick:** a 1 MiB file, a tree with `-r`, and
    `mv` of a file;
  - **Oceans stick → system disk:** `mv` of a directory;
  - **FAT → system disk:** `cp -r` of a directory;
  - **system disk → FAT:** `mv` of a directory and a 1 MiB file into a
    directory;
  - **refusals:** a directory without `-r`, into itself, the same file,
    `rm -r` of a mount point;
  - `rm -r` of a tree;
  - the host compares every copied file byte for byte, finds the moved
    sources gone, and runs `fsck.fat` on the FAT stick.
- **Failure behaviour:**
  - errors name the side that failed;
  - a cross-filesystem `mv` removes the source only after its copy is
    durable;
  - if the removal then fails, `mv` says the source was copied but not
    removed.
