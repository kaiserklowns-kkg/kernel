# ADR-0140: Ethernet at full speed, and back after a cable comes out

- Status: Accepted
- Date: 2026-10-14
- Depends on: ADR-0041 (Intel Ethernet), ADR-0123 (Ethernet for real PCs)
- Part of Phase 10 (Alpha).

## Context

On the first real machine (an HP ZBook with an I219-V), installed over
Debian, two problems showed up:

- The link came up only at 10 Mb/s, on a gigabit router.
- After a `link down` it never came back: updates failed with
  `fetch: timed out` until the machine restarted.

Two causes:

- **Speed:** on power-down Linux sets the PHY for Wake on LAN, with
  low-power link-up (LPLU: the lowest speed both ends have) and gigabit
  off. This is in the MAC's `PHY_CTRL` and in the PHY's OEM bits
  (`HV_OEM_BITS`, page 0 register 25). The PHY keeps them while standby
  power lasts, through a restart into another system. Linux undoes them
  when it starts; Oceans did not.
- **Recovery:** the driver only logged a link change. The service loop
  ran on interrupts alone, so a PHY that gave up negotiating stayed down.

## Decision

- **Full speed while running (I217, I218, I219):**
  - After bring-up, the driver clears D0 low-power link-up and gigabit-off
    in `PHY_CTRL` and in the OEM bits.
  - Autonegotiation then restarts, unless the ME blocks PHY resets.
  - If the bits were set, it says so once:
    `low-power link-up was on (PHY_CTRL …, OEM …): off`.
- **A tick for every NIC driver:** `oceans-nic` services a driver with an
  interrupt every second too (`TICK_MS`), not only when it raises one.
  Without an interrupt, it still polls every 10 ms.
- **Relink:** while the link stays down, `e1000e` restarts
  autonegotiation every 5 s. It says so once each time the link goes
  down: `link still down: autonegotiating again`. A link that comes back
  is logged as before (`link up, …`).

## Consequences

- The ZBook should link at 1000 Mb/s (or what the cable allows) and come
  back after a cable is unplugged and plugged in again. That needs the
  next update on the real machine. QEMU's e1000e (an 82574L, link always
  up) exercises only the tick, in every smoke test.
- The other drivers (`igc`, `rtl8168`) get the tick but no relink yet.

## Alternatives considered

- **Forcing 1000 Mb/s:** it breaks 100 Mb/s switches, and gigabit cannot
  be forced on copper.
- **Resetting the whole device on link down:** it drops the rings and
  what is queued, for what a PHY restart does.

## Checklist (master spec §48)

- Purpose: a usable wired link on real PCs.
- Architecture: the driver and the NIC library.
- API: none.
- Dependencies: none.
- Security: none (the same registers as bring-up).
- Testing: the smoke tests, and the ZBook in the next update.
- Failure: a PHY that does not answer is logged, and the driver goes on
  as before.
