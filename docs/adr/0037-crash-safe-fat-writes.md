# ADR-0037: Crash-safe FAT writes

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0036 (read-only FAT), ADR-0035 (removable media)
- Amends: ADR-0036 (FAT volumes become writable)

## Context

ADR-0036 reads sticks formatted elsewhere. Writing them is the other
half: saving a download onto a friend's stick, or editing a file a camera
made.

FAT has no journal and no copy-on-write. An update touches up to four
places, in any order a careless writer chooses:

- the data;
- the first FAT copy;
- the second FAT copy;
- the directory entry.

USB sticks are pulled out mid-write, and drives reorder writes in their
caches. A crash between those updates can:

- cross-link two files;
- point a file into clusters that are free (and later reused);
- expose garbage beyond the data written.

Other systems' checkers then "repair" by deleting things.

## Decision

- **Ordered writes with barriers.** The `Disk` trait gains `write_at`
  (whole 512-byte sectors), `flush` (a barrier: everything before reaches
  the medium before anything after; the USB stick's SYNCHRONIZE CACHE)
  and `writable`. Every change is a sequence whose every prefix, with
  any subset of the writes since the last barrier, is consistent.
  - **Write / extend:**
    1. Data goes into the file's clusters (beyond its size, still
       invisible) and into newly found free clusters.
    2. The new clusters' own chain is written and ended — barrier.
    3. The chain is linked from the file's old tail — barrier.
    4. The directory entry's first cluster and size are written last.
  - **Shrink:**
    1. The entry's size is written first (an empty file names no
       cluster) — barrier.
    2. The chain is ended at the new tail — barrier.
    3. The remainder is freed.
  - **Create:**
    1. A directory's cluster is prepared (`.`, `..`, the rest empty) and
       allocated — barrier.
    2. The long-name entries are written — barrier.
    3. The short entry, which makes it exist, is written last.
    - A full directory grows by a zeroed cluster, ended, then linked.
  - **Remove:**
    1. The short entry, then its long-name entries, are marked deleted —
       barrier.
    2. The clusters are freed: at once, or, for a file still open, when
       its last handle goes (Unix semantics).
  - **Invariants:**
    - no entry names a free cluster;
    - no chain passes through a free cluster or shares one;
    - no size covers clusters the file lacks.
  - **What a crash can leave:**
    - lost clusters (allocated, unreferenced);
    - orphaned long-name entries;
    - a chain longer than its file needs;
    - FAT copies that differ.
  - **Not atomic:** overwriting existing bytes of a file in place. A crash
    can leave a mix of old and new bytes there, as on every non-COW
    filesystem.
- **The clean bit and repair:**
  - Before the first change of a session, FAT[1]'s clean bit (FAT16/32)
    is cleared, durably. `sync` sets it again and refreshes FSInfo (free
    count, next-free hint), so a synced stick is clean for every system.
  - `enable_writes` checks the volume before allowing any writes:
    - **Clean volume:** the free count comes from FSInfo, falling back
      to a FAT scan.
    - **Unclean volume (and always for FAT12, which has no bit):**
      1. the first FAT is copied over the others where they differ;
      2. every directory and chain is walked;
      3. lost clusters are freed, orphaned long-name entries deleted, and
         over-long chains trimmed (in the same order as a shrink);
      4. FSInfo is rewritten and the volume marked clean.
    - Damage our ordering cannot cause (shared clusters, a file shorter
      than its size, links to free or bad clusters) leaves the volume
      **read-only**, with the reason logged: repairing that is guesswork,
      better done with the volume's owner's tools.
  - `check` reports the same findings without changing anything.
- **Names:**
  - Validated as Windows does: no `"*/:<>?\|` or control characters, no
    trailing dot or space, at most 255 UTF-16 units.
  - An exact 8.3 name in one case per part is stored short, with the NT
    lowercase flags.
  - Anything else gets long-name entries and a unique `BASIS~N` short
    name.
  - Timestamps come from the wall clock (ADR-0031).
- **FAT copies:** both are written. With FAT32 mirroring disabled, only
  the active one.
- **The fs service** mounts FAT read-write when `enable_writes` succeeds:
  `mounted a FAT16 volume "OCEANS16", read-write`, with a summary of any
  repair. Otherwise it stays read-only and says why. Commit (sync, or
  closing a written file) calls `Fat::sync`.
- **`fs-proto` `walk` fix:** opening a path for writing opens the
  directories on the way writable too. Writes deeper than one level
  (through a mount point, or nested on the system disk) had been refused.

## Testing

- **Writing** (on the three reference images: FAT12 superfloppy, FAT16
  in MBR, FAT32 in GPT):
  - creates with short, mixed-case, long and Thai names, and nested
    directories;
  - writes in pieces of 7 to 4096 bytes, overwrites, zero-filled gaps,
    shrinking, emptying, refilling, growing by truncation;
  - removals, including non-empty directories (refused) and files
    removed while open;
  - twelve `~N` names in one directory; filling a volume to `NoSpace`
    with nothing leaked.
  - A fresh mount reads everything back, and `check` finds no lost
    clusters and the exact free count.
- **Crashes:** the whole scenario runs on a recording disk. For every
  barrier epoch, the test builds the image of a crash with none, or a
  random part, of that epoch's writes (the device may reorder within an
  epoch). That is hundreds of states per image.
  - Every state must mount, repair, pass `check` with nothing left to
    repair, and read every file in full.
  - All writes together give exactly the expected tree.
- **Reference tools:**
  - `fsck.fat -n` must find nothing on the written images and on sampled
    crash states after repair;
  - `mtype` must read our long-named, Thai-named and nested files byte
    for byte.
  - CI installs dosfstools and mtools. Locally, WSL can run them via
    `OCEANS_FAT_TOOLS_WSL`; without them, this part is skipped with a
    note.
  - The tools caught two real gaps during development, both now
    repaired: orphaned long names, and a stale FSInfo after repair.
- **Smoke:** after the FAT16 stick is hot-plugged, the shell writes a
  file, makes a directory, writes inside it and removes a file. fetch
  saves 1 MiB, and `sync` follows. The host reads it all back with
  oceans-fat, requires `check` to be spotless, and runs `fsck.fat` (in
  CI always).
- `oceans-fat` is optimised in dev builds too. The crash tests run in
  about 10 s.

## Consequences

- **Sticks from other systems can be written,** and a stick pulled out
  mid-write needs at most the repair we do ourselves on the next mount.
  Windows' and Linux' checkers find nothing, because we clear what they
  would complain about.
- **Each change costs extra flushes:** two to four barriers per write
  call. Bulk writes come in 16–64 KiB calls (shared buffers), so the
  overhead per byte is small. Saving 1 MiB onto the QEMU FAT stick takes
  about 2.4 s.
- **Still not supported:** exFAT, renaming (fs-proto has no rename yet),
  writing volume labels, and 4K-sector media.

## Alternatives considered

- **Write-back caching of FAT sectors:** faster, but ordering then
  depends on flush logic far from each operation, which is harder to
  prove. Ordering is kept explicit.
- **Shadowing the FAT (writing FAT2, barrier, then FAT1):** this guards
  against a torn FAT sector, but sector writes are atomic on the media we
  serve and other systems read FAT1 regardless. Mirroring repair after a
  crash covers the copies differing.
- **Using the clean bit only (no repair), like many embedded stacks:**
  this leaves lost clusters and orphans for the next Windows check to
  flag.

## Checklist (master spec §48)

- **Purpose:** writing FAT sticks safely.
- **Architecture:** ordered writes with barriers in `oceans-fat`; repair
  on mounting unclean volumes; served through the same fs `Store`.
- **API:**
  - `Fat::{enable_writes, write_file, truncate, create, remove, sync,
    check, set_clock}`, `Recovery`, `Report`;
  - `Disk::{write_at, flush, writable}`;
  - fs-proto `walk` keeps write access through directories.
- **Dependencies:** none (dosfstools and mtools only verify, in tests and
  CI).
- **Security:**
  - every on-disk value is still checked;
  - damage beyond our crash model leaves the volume read-only rather
    than being "repaired";
  - names are validated before they reach the disk.
- **Failure behaviour:**
  - errors mid-operation leave a state the next mount repairs;
  - `NoSpace` allocates nothing;
  - a read-only disk mounts read-only.
