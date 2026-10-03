# ADR-0035: Mounting removable media

- Status: Accepted
- Date: 2026-10-03
- Depends on: ADR-0019 and ADR-0022 (filesystem), ADR-0034 (USB mass storage)

## Context

ADR-0034 made a USB stick a block device (`usbdisk`). People do not want
blocks; they want to save a download to the stick, look at what is on it,
and carry it to another machine. Two things were missing:

- **A filesystem service that copes with a disk that comes and goes.**
  It must mount when a stick is plugged in, and never write the volume
  it remembers onto a different stick plugged into the same port.
- **One file tree.** Programs receive a single filesystem root (`use:fs`).
  A stick has to show up inside that tree, or every program would need a
  second grant and its own idea of where the stick is.

## Decision

- **Removable media mode in the fs service.** The same image runs as a
  second service, `usbfs`, with `use = usbdisk as media`.
  - **Mounting on demand:** when a request arrives and nothing is
    mounted, it opens a session on the block service and mounts the
    volume.
    - A blank disk is formatted.
    - A disk holding anything else (FAT, for example) is left untouched,
      and requests answer `Unsupported`, logged once per disk.
    - With no disk, requests answer `NoMedium`.
  - **Before every request**, it checks that its session still reaches
    its disk (`Disk::alive`, one `INFO` call). If not, or after any I/O
    error, it unmounts: buffers are unmapped, the open-handle table is
    dropped, and the volume is forgotten.
    - Handles opened on the old volume answer `NoMedium` from then on.
      Badges are never reused, so an old handle never reaches the new
      volume.
  - **usb-storage ends every session of a stick that goes away.** A
    stale session cannot reach the next stick, even when it is plugged
    into the same port. A volume is therefore only ever written to the
    disk it was read from.
  - Durability is unchanged (ADR-0022). Changes are committed atomically
    on `SYNC`, on creates and removes, and when a writing handle closes.
    Unplugging loses at most uncommitted changes and never corrupts the
    volume. Safe removal is `sync`.
- **Mount points in the system fs (VFS-style forwarding):**
  - A grant `use = usbfs as mount:usb` makes `/usb` a directory of the
    root. It is listed first and cannot be removed. A real entry of
    the same name can no longer be opened (it is still listed).
  - Opening it gives the client one of our handles, standing for the
    mounted service's root. Every request on such a handle is forwarded
    with `ipc_call_msg`, and the reply passed back.
    - A handle in the reply (from `OPEN`) is wrapped in a new handle of
      ours.
    - Capabilities in the request (an `ATTACH` buffer) are passed on, so
      shared buffers (ADR-0030) work through mounts.
    - Closing our handle closes the forwarded one.
  - Write access below a mount point follows the same rule as
    everywhere: requests that change anything are refused locally unless
    the handle was opened for writing through a writable parent.
  - `SYNC` on the system fs also syncs every mount, so the shell's
    `sync` makes the stick safe to remove.
- **init:** a `use` grant may carry an alias, `use = NAME as ALIAS`. The
  endpoint is NAME and the service's directory lists it as ALIAS. The
  grant keeps a single string, so init's fixed tables still fit its
  64 KiB stack.
- **fs-proto:** new statuses `NoMedium` ("no disk") and `Unsupported`
  ("not an Oceans volume").

## Consequences

- **Every program sees the stick.**
  - The shell: `ls /usb`, `write /usb/…`, `cat /usb/…`.
  - fetch: `fetch URL /usb/file`.
  - Anything else holding `use:fs`.
  - No program changed.
- **Speed:** requests below a mount point cost one more IPC round trip.
  The system fs waits during each forwarded request, so a slow stick
  delays other filesystem clients meanwhile. Bulk data still moves
  through shared buffers.
- **The mount table is static:** mount points come from the manifest.
  Which disk is mounted is dynamic.
- **No FAT:** sticks formatted elsewhere are refused, untouched. A
  read-only FAT service mounted the same way would make them readable.
- **Formatting is automatic:** a blank stick is formatted with no
  confirmation. Blank means all zeroes, so nothing is lost.

## Alternatives considered

- **A namespace in the shell:** the shell resolves `/usb` itself. But
  programs it runs would see a different tree, and every client would
  need the same logic.
- **The stick's volume inside the system fs process:** this needs block
  access to the stick in the system fs, mixes two failure domains, and
  makes removal handling part of the core filesystem.
- **Mount and unmount operations at run time:** more flexible, but they
  need a policy for who may mount what. Static mount points plus dynamic
  media cover removable storage.
- **Re-mounting by matching volume identity:** a stick unplugged and
  replugged could keep its old handles alive. That requires a volume
  identifier and handle migration. Today the handles end, and reopening
  is cheap.

## Checklist (master spec §48)

- **Purpose:** files on USB sticks, in the one tree every program sees.
- **Architecture:**
  - the fs service in removable-media mode (`usbfs`);
  - mount points in the system fs that forward requests;
  - init aliases.
- **API:**
  - `/usb`;
  - statuses `NoMedium` and `Unsupported`;
  - `Disk::alive`;
  - `use = NAME as ALIAS`;
  - the grant names `media` and `mount:NAME`.
- **Dependencies:** none.
- **Security:**
  - write access through mounts follows the parent handle's access;
  - a volume is never written to a disk it was not read from;
  - foreign disks are never modified;
  - forwarded capabilities move as in any request.
- **Testing:**
  - The smoke test starts with a blank stick: `/usb` is listed, the
    stick is formatted on first use, and the shell writes and reads
    `/usb/note.txt`.
  - fetch saves 1 MiB to `/usb/big.bin` through shared buffers, then
    `sync`.
  - After the stick is unplugged, usbfs unmounts and `ls /usb` reports
    "no disk".
  - The second boot mounts the stick again and reads both files.
  - The host opens the stick image as an Oceans volume and checks both
    files byte for byte.
- **Failure behaviour:**
  - a removed or failing disk is unmounted, and requests answer
    `NoMedium` until a disk is mounted again;
  - a mounted service that dies makes forwarded requests answer
    `IoError`.
