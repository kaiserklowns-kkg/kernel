# ADR-0130: Choosing a Wi-Fi network in Control Center

- Status: Accepted
- Date: 2026-10-13
- Depends on: ADR-0129 (Wi-Fi), ADR-0118 (Control Center), ADR-0053
  (directory grants), ADR-0114 (keyboard layouts), ADR-0117 (the
  wallpaper)
- Part of Phase 10 (Alpha).

## Context

With ADR-0129 a network is set by writing two files from the shell. A
desktop user picks a network from a list and types its password, as on
every other system. The desktop (`display`) has no network authority and
should get none for this: it needs to see what the driver sees and to
say which network to join, nothing more.

## Decision

### The driver and the desktop share files, not a protocol

`/system/wifi`, granted to both (`grant = storage:/system/wifi`), one fact
per line, readable from the shell (`oceans_wifi::state` has the formats,
host-tested):

| File | Written by | Holds |
|---|---|---|
| `status` | iwm | `off`, `idle`, `scanning`, `joining SSID`, `joined SSID RSSI CHANNEL`, `failed SSID reason` |
| `networks` | iwm | the last scan, by name, strongest first: `W -48 6 SSID` (O open, W WPA2, U not supported) |
| `scan` | the desktop | asks for a scan; iwm removes it |
| `password` | the desktop | the passphrase (removed for an open network) |
| `ssid` | the desktop | the network to join (removed: leave) |

Names are hex in iwm's files: an SSID may hold any of 32 bytes. The
desktop writes `password` first, then `ssid`, which iwm acts on; a
half-written pair costs one failed attempt, retried at once when the
setting changes (ADR-0129). iwm writes `status` only when it changes.

### The panel

Control Center's network toggle opens the Wi-Fi panel in Control
Center's place when `status` exists (else Settings, as before), and asks
for a scan:

- back, the title, Scan;
- what Wi-Fi does: "Connected to …", "Joining …", "Could not join …:
  why", "Not connected", "Looking for networks…", "Wi-Fi is off";
- the networks, strongest first: signal bars, the name, WPA2, Open or
  Not supported; the one joined in bold, its bars in the accent,
  "Connected".

A click on an open network joins it; on a WPA2 one, asks its password:
dots, never the characters; 8 to 63 printable ASCII characters (the
keyboard layout's, ADR-0114); Enter joins, Escape cancels. A click on the
network joined leaves it. Closing Control Center drops a half-typed
password. While the panel is open its files are read every 500 ms, else
every 2 seconds. Control Center's status line says "Wi-Fi NAME" instead
of "Ethernet" when Wi-Fi carries the network.

### The wallpaper becomes a module

The panel took `display` over 4 MiB, the most the kernel loads a program
in (its heap's largest block; the ABI allows 16 MiB, to be fixed). The
3.4 MB ocean photograph (ADR-0117) moves out of the program into a
module of the boot image, `wallpaper-ocean.png`, granted to `display`
and mapped while it is decoded; `display` is 0.8 MB. Without it the
drawn wallpaper shows, as before when the picture could not be read.

## Consequences

- **A network is chosen with the pointer and the keyboard**, and the
  shell's way still works, since both use the same files.
- **The desktop gains one directory**, not the network: it cannot send or
  receive anything, and iwm reads only what the files say.
- **The password sits in a file** that the desktop, iwm and the shell's
  root can read; a keyring is later work.
- **Tested in QEMU without Wi-Fi:** the smoke test writes the driver's
  files from the shell, opens the panel, picks the network, types a
  password, checks the files written, and checks the screen once
  "joined".
- **Later:** a list longer than the panel (scrolling), hidden networks
  by name, forgetting a network, the Wi-Fi icon in the top bar.

## Alternatives considered

- **A control endpoint on iwm** (`provide = wifi`): the driver serves
  one endpoint in one thread (`oceans-nic`); a second protocol and a
  grant of it to the desktop for what files already carry.
- **Through Core:** Core would hold a Wi-Fi capability only to pass
  strings along.
- **Keeping the wallpaper in the program and fixing the kernel first:**
  the kernel's loader is a separate change; the module is right anyway (a
  picture is data, and a theme may change it).

## Checklist (master spec §48)

- Purpose: picking a Wi-Fi network on the desktop.
- Architecture: files in one granted directory between the driver and
  the desktop; the panel in `display`.
- API: no ABI change; the files' formats in `oceans_wifi::state`.
- Dependencies: `oceans-wifi`, `oceans-fs-proto` in `display`.
- Security: the desktop gets `/system/wifi` only; the password is
  shown as dots, dropped when Control Center closes, never logged.
- Testing: host tests of the formats; the desktop smoke drives the
  panel with the files faked; `smoke-hw` boots the hardware profile.
- Failure: no `status` file, no panel (Settings opens); a failed join
  shows why.
