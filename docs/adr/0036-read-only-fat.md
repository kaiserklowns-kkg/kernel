# ADR-0036: Read-only FAT

- Status: Accepted (amended by ADR-0037: FAT volumes are written too)
- Date: 2026-10-03
- Depends on: ADR-0035 (removable media)

## Context

Almost every USB stick arrives formatted as FAT32, and one written by
Windows, macOS, Linux, a camera or a phone is FAT (or exFAT). ADR-0035
mounts only Oceans volumes and refuses everything else, so such sticks
could not be read at all. Reading them is the common need: taking files
off a stick.

Writing FAT safely is harder:

- FAT has no journal, so an interrupted update can lose clusters or
  cross-link files.
- There are two FAT copies and the FSInfo sector to keep in step.
- Other systems expect particular long-name and short-name conventions.

This decision reads only.

## Decision

- **`libs/fat` (`oceans-fat`, no_std, host-tested): a read-only FAT12,
  FAT16 and FAT32 reader.**
  - **Locating the volume:**
    - a boot sector at the start of the disk ("superfloppy"), or
    - the first FAT-type partition of an MBR (types 01, 04, 06, 0B, 0C,
      0E, EF), or
    - the first Microsoft basic data or EFI system partition of a GPT.
  - **The boot sector is checked before use:**
    - sector size and sectors per cluster;
    - reserved sectors and FAT count;
    - that the volume fits the disk;
    - that the FAT covers every cluster.
    - The FAT type follows from the cluster count, as the specification
      defines it.
  - **Directories:**
    - VFAT long names are assembled from their entries and must match
      their short entry's checksum. They are decoded from UTF-16, so Thai
      and other scripts come through as UTF-8.
    - Without a long name, the 8.3 name is used, with the NT lowercase
      flags applied.
    - Deleted entries, volume labels, `.` and `..` are skipped.
    - Lookups ignore ASCII case, as FAT does, and match long or short
      names.
  - **Files:** read across cluster chains. A cursor remembers the last
    cluster reached, so sequential reads do not walk the chain from the
    start each time.
  - **Untrusted data:**
    - every link must name a real data cluster (free, reserved or bad
      links are `Corrupt`);
    - directory walks stop after as many clusters as the volume has;
    - file reads are bounded by the file size.
    - A looped chain can therefore return wrong data, but never hangs.
  - **Node numbers** follow `oceans-volume`'s pattern (root 0,
    lookup / retain / release). Unheld lookups reuse their slots.
  - Nothing is ever written: the reader has no write path.
- **The fs service (removable-media mode)** tries FAT when a disk is not
  an Oceans volume.
  - A FAT volume is served through the same handlers, behind a `Store`
    that is either an Oceans volume or a FAT one.
  - Every change answers `PermissionDenied`.
  - `SYNC` has nothing to do.
  - The log names the type and label, for example
    `mounted a FAT16 volume "OCEANS16", read-only`.
  - Disks that hold neither volume type are still refused, untouched.
- **Test images come from the reference tools, not from us:**
  - mkfs.fat and mtools build a FAT12 superfloppy, FAT16 in an MBR
    partition, and FAT32 in a GPT partition (`testdata/generate.sh`).
  - All three hold the same tree: short, long and Thai names, nested
    directories, an empty file and a fragmented one.
  - They are stored sparse (only non-zero sectors, about 30 KB each).
- **Shell:** `cat` and `echo` write whole chunks, so a log line cannot
  split a line of output.
- **Smoke harness:** typing also waits until the test crasher has stopped
  restarting. On Windows, QEMU drops typed bytes while the log is busy;
  one run lost half a command that way.

## Consequences

- Sticks from other systems can be read: `ls /usb`, `cat /usb/…`, and
  copies onto the system disk with any program.
- **No writing:** to write to a stick from another system, it has to be
  reformatted (blank) for Oceans. Writing FAT is future work with its own
  ADR, covering crash safety, both FATs and FSInfo.
- **No exFAT:** exFAT is common on sticks over 32 GB, but is a different
  format. Such sticks are refused, untouched.
- **Short names in OEM code pages:** non-ASCII bytes show as `_`. The
  long names, which modern systems always write, are exact.
- **Mount time:** mounting reads only the boot sector (and partition
  tables), so it is cheap.
- **Directory listings** scan from the start for each entry: quadratic,
  but fine for the directory sizes on sticks.

## Alternatives considered

- **A separate FAT service mounted elsewhere (`/fat`):** two names for
  one stick, and the user has to know the format. One mount point that
  serves whatever is there is simpler.
- **Read-write FAT now:** the useful half (reading) comes first, and
  writing needs careful crash semantics.
- **Tests with images built by our own code:** a writer and a reader
  written together can share the same misreading of the specification.
  The reference tools cannot.

## Checklist (master spec §48)

- **Purpose:** reading sticks formatted elsewhere.
- **Architecture:** a FAT reader library, served by the removable-media
  fs behind a common store interface.
- **API:**
  - `/usb` shows FAT volumes, read-only;
  - `oceans_fat::{Fat, Disk, FatType}`.
- **Dependencies:** none (the reference tools are needed only to
  regenerate test images).
- **Security:**
  - every on-disk value is range-checked;
  - walks and chains are bounded;
  - the volume is never modified.
- **Testing:**
  - 10 host tests on the three reference images:
    - all three layouts;
    - long, short, unicode and case-insensitive names;
    - nested paths and empty files;
    - fragmented files read in many piece sizes and backwards;
    - error cases;
    - node reuse;
    - refusing non-FAT, mislabelled and truncated disks;
    - corrupt chain links and loops, without hanging;
    - long-name checksums.
  - Smoke test: after the Oceans stick is unplugged, the FAT16 reference
    image is hot-plugged as a second stick and mounted read-only at
    `/usb`. The test lists the names (Thai included), reads files through
    a case-insensitive path, and has a write refused.
- **Failure behaviour:**
  - a damaged FAT volume is refused at mount (`Unsupported`), or
    reports `Corrupt` on the damaged file only;
  - unplugging works as in ADR-0035.
