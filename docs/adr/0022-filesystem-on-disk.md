# ADR-0022: The filesystem on disk (OceansFS)

- Status: Accepted (format amended by ADR-0027: data checksums)
- Date: 2026-10-03
- Depends on: ADR-0019 (filesystem service), ADR-0021 (block devices)
- Amends: ADR-0021. Raw block access moves from the shell to the
  filesystem.

## Context

The filesystem service (ADR-0019) kept everything in memory, so every
reboot lost every file. ADR-0021 brought a working disk. Persistence on
it must survive the realistic failure: power loss in the middle of a
write. An "fsck later" design is not acceptable for an OS that will store
models, packages and user data.

## Decision

### OceansFS: copy-on-write, committed atomically

A new host-tested, `no_std` crate, `oceans-volume`, holds the on-disk
volume. The fs service stays the protocol front end.

- **Layout.** The disk is an array of 4 KiB blocks:
  - blocks 0 and 1 are superblocks, used alternately (generation *g* goes
    to block *g* mod 2);
  - the rest are metadata and data blocks.
- **Metadata** is the whole tree, serialized in preorder:
  - per node: kind and name;
  - per file: size and block list (block 0 = a hole that reads as zeros);
  - per directory: child count.

  It is protected by a CRC-32C stored in the superblock, which carries its
  own CRC-32C.
- **No allocation bitmap.** Free blocks are whatever the metadata does not
  reference, recomputed at mount, so a bitmap can never disagree with the
  tree.
- **Copy-on-write.** A block the last durable generation references is
  never overwritten:
  - a write to such a block goes to a newly allocated block, which later
    writes rewrite in place until the next commit;
  - released blocks the durable state still references are freed only
    after the next commit.
- **Commit.** The steps are, in order:
  1. write the metadata to free blocks;
  2. flush;
  3. write the superblock for generation *g+1*;
  4. flush.

  A power cut at any point leaves the old generation or the new one, never
  a mix. A torn superblock fails its CRC, and the other slot holds the
  previous durable generation, which is still intact.
- **Mount uses only the newest valid superblock.** If that generation does
  not verify, the volume is reported corrupt and is **not** silently
  rolled back: older generations may reference blocks reused since.
- **Space for the commit is reserved up front.** Every allocation first
  ensures that the next commit's metadata will still fit. A full disk
  therefore still commits, and a shrink does no I/O. (Stale bytes past a
  partial last block are zeroed when the file grows again.)
- **Limits:** 4 KiB blocks, files up to 16 MiB, 4096 nodes, 32 directory
  levels and 128-byte names. They are enforced at creation and again at
  mount, so the service never writes a volume it would then refuse.

### Durability

| Change | Durable when |
|---|---|
| Creating or removing an entry | before the call returns |
| File contents | on `SYNC` (new protocol op 8; shell `sync`), or once the service has processed the close of the handle that wrote them. The close returns before that commit; see ADR-0028. |
| Anything, on clean shutdown | the service commits when its endpoint closes |

A crash loses at most the uncommitted writes of files still open. It
never corrupts the volume.

### Mounting policy

The fs service gets `use = block` in `services.conf` and opens a session
with a one-block buffer.

| Disk | Action |
|---|---|
| Blank (superblock area all zeros) | formatted |
| A valid OceansFS volume | mounted |
| Anything else, or a damaged volume | **left untouched**, logged; files kept in memory |
| No disk or no block service | files kept in memory |

The system never formats a disk that holds data.

### Volatile nodes and `/bin`

- `/bin` is published from boot modules at every boot. It is a
  **volatile**, read-only directory: never written to disk, always
  current with the boot image.
- Without a disk the whole volume is volatile, which is the previous
  in-memory behaviour.

### Authority

- **The filesystem is now the only holder of raw disk access.** The shell
  loses `use = block`: raw sector writes would bypass every file
  permission and corrupt the mounted volume.
- `disk` remains in `/bin` for diagnostics where `services.conf` grants
  block access. From the shell, `run disk out use:block ...` now says the
  shell does not hold it.

## Consequences

- Files survive reboots and power loss. `cargo xtask run` keeps them in
  `build/disk.img`.
- Every commit rewrites the whole metadata. That is cheap at this scale
  (tens of KiB for thousands of nodes) and keeps the format simple and
  verifiable. A tree of per-directory metadata blocks is the path when
  volumes grow.
- Data blocks have no checksums, so bit rot in file contents is not
  detected (metadata is protected). Per-block checksums are a format
  version 2 candidate.
- One disk, one volume, no partitions yet.
- If the block driver restarts, the fs service keeps its old, dead
  session and falls back to errors until it is restarted too. Reconnecting
  is future work.
- **xtask drives the smoke shell one command per prompt.** QEMU's Windows
  stdio backend ignores backpressure, so keystrokes sent during a disk
  flush overflowed the UART.

## Alternatives considered

- **FAT or ext2.** They are compatible with other systems, but neither is
  crash-consistent without a journal or fsck, and both carry decades of
  format complexity. Interchange can come later as a separate service.
- **Journaling (write-ahead log).** It is crash-safe too, but needs a
  replay path and in-place updates. Copy-on-write with a superblock flip
  has a single commit point and no replay.
- **Snapshotting the whole in-memory tree to disk.** That is simple, but
  every commit would rewrite all file data.

## Testing

- `cargo test -p oceans-volume` (16 tests), on an in-memory disk:
  - round trip through remount, with free space recomputed exactly;
  - uncommitted changes are not durable;
  - **crash replay:** the writes of a commit sequence (overwrite, delete,
    create, nest) are replayed up to every possible point, whole and torn.
    Each prefix must mount as exactly the old or the new state.
  - copy-on-write never writes a committed block, and fresh blocks are
    rewritten in place;
  - holes, truncation and zeroed tails;
  - a full disk still commits;
  - deletion frees blocks after commit;
  - open-but-unlinked files stay readable;
  - volatile nodes are never written;
  - memory-only volumes;
  - limits at creation and at mount;
  - I/O errors leave a consistent volume;
  - corrupt metadata and superblocks are refused (never reformatted);
  - the parser survives every single-byte mutation of real metadata, and
    duplicate or out-of-range blocks are rejected;
  - CRC-32C check value.
- **Smoke test: two boots on one blank disk.**
  - Boot 1 must log `fs: formatted a blank disk`. It creates
    `/keep/note.txt`, creates and removes `gone.txt`, and syncs.
  - Boot 2 must log `fs: mounted the disk`. It reads the note back, lists
    `/keep` and `/` (including `docs/` from boot 1), and checks that `/bin`
    is still read-only.
  - **xtask then mounts the image file on the host with `oceans-volume`**
    and checks the note's exact contents and that `gone.txt` is gone.
  - Passes in debug, release and with `-cpu max`.
- Checked by hand: three normal boots, each ended by killing QEMU instead
  of shutting down. A file written in boot 1 is read back in boot 2;
  removed in boot 2, it is gone in boot 3.

## Checklist (master spec §48)

- **Purpose:** persistent files.
- **API:** fs protocol op `SYNC` and status `IoError`; otherwise
  unchanged.
- **Dependencies:** none external.
- **Security:**
  - disk contents are validated as untrusted input;
  - raw disk access is held only by the filesystem;
  - foreign or damaged disks are never written.
- **Failure behaviour:**
  - power loss: old or new state;
  - I/O errors: `IoError`, and the volume stays consistent;
  - an unusable disk: memory only, logged.
