# ADR-0027: OceansFS format 2, data block checksums

- Status: Accepted
- Date: 2026-10-03
- Amends: ADR-0022 (on-disk format)

## Context

ADR-0022 protected the superblocks and the metadata with CRC-32C, but not
file contents. A disk (or its firmware, cable or controller) that returns
wrong data would hand corrupted file contents to programs as if they were
good. ADR-0022 named this the format 2 candidate.

## Decision

- **Format 2:** each file block in the metadata is a block number **and
  the CRC-32C of its 4 KiB contents**. Holes (block 0) have none. The
  superblock records the format version.
- **Every data block read from the disk is verified.** A mismatch returns
  `Corrupt`, never the data. The fs protocol reports it as status 11, "data
  corrupted on disk (checksum mismatch)".
  - Blocks in the cache were verified on the way in, or written by us, so
    hits cost nothing.
  - A partial write of a damaged block also fails: it reads the block
    first, so it never re-checksums corrupt data as good.
- **Writes** compute the checksum of each block as it goes to the disk.
  Copy-on-write is unchanged: a block and its checksum change together in
  the next commit's metadata.
- **Format 1 volumes still mount.**
  - At mount their data checksums are computed from the disk.
  - The volume is marked dirty, so the next commit writes format 2.
  - Mounting never writes (the host-side checker in xtask mounts
    read-only).
- **Cost:** metadata grows from 4 to 8 bytes per block, and one CRC per
  4 KiB block read or written.

## Consequences

- Silent corruption becomes a reported error at the file that has it.
  Nothing can repair it yet; that needs redundancy (a mirror, or parity),
  which this format can carry later.
- A format 1 volume corrupted before the upgrade gets checksums of its
  corrupted data: the guarantee starts at the upgrade.

## Testing

- `cargo test -p oceans-volume` (18 tests):
  - **bit rot:** one flipped bit in a data block on the disk; the intact
    block reads fine, while the damaged one returns `Corrupt` for both a
    read and a partial write;
  - **upgrade:** a volume written as format 1 mounts, reads correctly,
    is dirty, commits as format 2 (checked in the superblock) and mounts
    again.
  - The crash replay (every prefix of a commit, whole and torn) and every
    other ADR-0022 test pass on format 2.
- **Smoke:**
  - the two-boot test runs on format 2;
  - xtask's host-side mount verifies every block it reads.
