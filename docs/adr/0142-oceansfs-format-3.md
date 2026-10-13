# ADR-0142: OceansFS format 3: runs, CRC tables, and a bit a block

- Status: Accepted
- Date: 2026-10-14
- Depends on: ADR-0022 (OceansFS), ADR-0027 (data checksums), ADR-0134
  (files up to 256 MiB), ADR-0126 (installed on the internal disk)
- Part of Phase 10 (Alpha).

## Context

Installed on a real laptop, the root volume takes most of a 512 GB SSD:
about 125 million blocks. Format 2 did not scale to that:

- **Memory:** a byte of state for every block, about 120 MiB in the
  filesystem service.
- **Commits:** every commit walked every block's state, and wrote the
  whole metadata. The metadata listed every block of every file with its
  CRC (8 bytes each).
- **Capacity:** the metadata may take 1011 blocks (about 4 MiB), so the
  volume could hold only about 2 GiB of file data in all.
- **Allocation:** a block was found by walking states one at a time.

## Decision

Format 3 (the superblock's version; formats 1 and 2 still mount):

- **Runs.** A file's blocks are kept as runs: file block, disk block and
  length, for stretches in consecutive disk blocks. A file block in no
  run is a hole. A file written in order is one run, however long:
  - a block is allocated on from the file's block before it;
  - an overwrite of a committed block moves only that block, splitting
    the run, and the pieces join again if the block comes back next to
    them.
- **CRC tables.** The CRC-32C of each data block (ADR-0027) moves out of
  the metadata into tables: a block of 1024 CRCs for each 1024 file
  blocks (4 MiB of file).
  - The metadata keeps each table's block and that block's own CRC
    (block 0: no table yet, its file blocks all holes).
  - Tables are read when needed and checked against their CRC. A
    damaged table is reported (`Corrupt`), like a damaged data block.
  - A commit writes only the tables that changed, to new blocks
    (copy-on-write, before the metadata). The old blocks are freed with
    the old generation.
  - Clean tables are let go once more than 4096 are in memory (16 MiB).
- **A bit a block.** Free and used blocks are a bitmap in memory, found
  at mount from the metadata as before: 15 MiB for 125 million blocks.
  The blocks allocated or released since the last commit are kept apart
  (`Blocks`), so a commit costs what changed. Allocation searches the
  bitmap a word at a time.
- **Upgrade.** Mounting formats 1 and 2 makes the runs and the tables
  (for format 1, from the data, as before). The next commit writes format
  3.
- **Validation.** Mount still checks everything:
  - runs sorted, not overlapping, inside the file, on valid blocks, each
    block referenced once;
  - one table per 1024 file blocks;
  - no file block with data where its table is missing.

## Consequences

- A 256 MiB file written in one go takes about 530 bytes of metadata
  (format 2: 512 KiB). The metadata now grows with runs and tables, not
  blocks: about 1 TiB of files written in order fits.
- On the ZBook's volume the service needs about 15 MiB for blocks, not
  120 MiB.
- Host tests (30), beside the existing ones (a crash at any write, a torn
  superblock, copy-on-write, corruption, I/O errors):
  - a 6 MiB file is one run, metadata under 64 bytes;
  - an overwrite splits a run;
  - format 2 volumes upgrade without changing anything or leaking a
    block;
  - a damaged table is reported;
  - a data block without a table is refused;
  - the bitmap and the runs have their own tests.
- **Going back:** a release before this one refuses a format 3 volume
  ("unsupported format version"). After this update, the boot menu's
  "previous" (ADR-0122) starts without the root volume. Recovery
  (ADR-0122) still starts.
- Not yet:
  - metadata that grows past 4 MiB (an indirect list of its blocks);
  - a free-space map on disk (mount still reads the whole tree).

## Alternatives considered

- **A CRC per run:** a read of one block would read the whole run to
  check it.
- **Keeping CRCs in the metadata, made indirect:** the metadata would
  still be rewritten whole at every commit.
- **A B-tree of extents (as Btrfs, APFS):** the lasting design for
  millions of files. Format 3 changes how a file's blocks are kept, not
  the tree, which a later format can.

## Checklist (master spec §48)

- Purpose: a volume the size of a real disk.
- Architecture: `oceans-volume` (`blocks.rs`, `runs.rs`).
- API: unchanged.
- Dependencies: none.
- Security: mount validates every run and table; tables are checksummed
  like data.
- Testing: host tests, and every smoke test (the root is format 3).
- Failure: a damaged table is `Corrupt`; a format 3 volume is refused by
  older releases.
