# ADR-0134: Files up to 256 MiB

- Status: Accepted
- Date: 2026-10-13
- Depends on: ADR-0022 (OceansFS), ADR-0027 (data checksums),
  ADR-0071 (updates)
- Part of Phase 10 (Alpha).

## Context

On the first real machine an update package (about 30 MiB) could not be
saved to `/home`: `fetch: no space`. OceansFS refused any file larger than
16 MiB, so updates had to go through the EFI system partition, and a
video or a disk image could not be kept at all.

The limit is not in the format. A file's metadata is its size and one
entry (block, CRC-32C: 8 bytes) per 4 KiB block; the metadata may take
1011 blocks, about 4 MiB.

## Decision

`MAX_FILE_SIZE` is 256 MiB. Such a file's block list is 512 KiB, an
eighth of the metadata. The format is unchanged (still version 2), so
nothing is converted.

## Consequences

- Updates are fetched to `/home` and applied from there.
- A host test writes past 40 MiB (a hole in between), remounts and reads
  it back, and growing past 256 MiB fails with `NoSpace`.
- A release before this one refuses to mount a volume holding a file over
  16 MiB ("bad file size"): going back to such a release (the boot menu's
  "previous", ADR-0122) needs those files removed first. Alpha releases
  are not kept for rollback beyond one step, and an update file is
  removed once applied.
- Still: about 2 GiB of file data in all, because the metadata is one
  block list rewritten at every commit. A disk beyond that needs extents
  (runs of blocks) in a format 3; not yet.

## Alternatives considered

- **Extents now:** the lasting fix for the total, but a format change
  with a conversion; the per-file limit needed only a constant.
- **Keeping updates on the EFI system partition:** it is small (often
  100–300 MiB) and shared with other systems.

## Checklist (master spec §48)

Purpose: large files · architecture: a constant · API: none ·
dependencies: none · security: mount still checks every size against the
limit · testing: a host test · failure: `NoSpace`, as before.
