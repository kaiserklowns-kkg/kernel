# ADR-0146: Oceans' promises to the user, and a network that says where it went

- Status: Accepted
- Date: 2026-10-14
- Depends on: ADR-0007 (AI as a mediated principal), ADR-0047
  (permissions), ADR-0070 and ADR-0074 (the kept log), ADR-0071
  (updates), ADR-0143 (network time)
- Part of Phase 10 (Alpha). ABI 25.
- Plan: [development/plan-lessons-from-other-systems.md](../development/plan-lessons-from-other-systems.md)
  (WP-1, WP-9, WP-10); research:
  [architecture/lessons-from-macos-windows-linux.md](../architecture/lessons-from-macos-windows-linux.md).

## Context

People leave Windows, macOS and Linux over the same complaints, and none
of them is a missing feature:

- **Windows:** diagnostic data that cannot be turned off, ads in the
  system, a Microsoft account forced at setup, restarts forced for
  updates, Recall recording the screen, and Edge taking back the default
  browser.
- **macOS:** permission prompts that come back every month, hardware cut
  off by policy, and looks put before legibility.
- **Linux:** working on the wrong laptop and not finding out until it is
  installed.
- **All three:** a bad driver or update can stop the machine.

Oceans' architecture already avoids many of these. Its kernel is
capability-based, its drivers run in userspace, it updates through two
slots, its AI has no ambient authority, and its clipboard is pushed only to
the app the user pastes into. Nothing so far *states* these things as
commitments, or checks that a later change keeps them.

## Decision

### The promises

Every later ADR's checklist says which of these it touches
(`template.md`).

| # | Promise | Kept by |
|---|---|---|
| P1 | **No telemetry, no ads.** With no one asking, the system reaches the network only for what the user can see and turn off: network time (ADR-0143), and checks for updates once they exist (WP-4) | The smoke check below |
| P2 | **No online account is needed.** Accounts are local (ADR-0110, 0115) | Setup and Settings have no sign-up |
| P3 | **The machine is the user's.** Nothing restarts, updates or reinstalls without the user. Defaults the user chose stay chosen (ADR-0099) | `update` is run by the user (ADR-0071); WP-4, WP-8 |
| P4 | **AI never watches.** No AI tool reads the screen or indexes in the background. Each read is approved, and each action is logged (ADR-0007, 0051, 0055) | Review of every AI tool |
| P5 | **Every permission is visible and revocable,** asked once, not nagged (ADR-0047, 0081) | WP-2 |
| P6 | **A failure is contained and can be undone.** A driver is a process. An update has a previous slot, and safe mode comes by itself (ADR-0021, 0071, 0122) | WP-5 |
| P7 | **Nothing moves without a reason.** A change the user sees is written in its ADR | The template's section "What the user sees change" |
| P8 | **Know before you install,** and how long a machine is supported | `support-tiers.md`; WP-3 |
| P9 | **Anyone can check a release.** Builds are signed (ADR-0072, 0075) and dependencies are few (ADR-0004) | WP-6 |
| P10 | **Legibility before effects** | WP-7 |

### The network says where it went, and does not keep it (ABI 25)

- **`DEBUG_WRITE` takes a level** in its fourth argument:
  - `log_level::INFO` (0) is what every line was before ABI 25. It is kept
    in the ring (ADR-0070), on disk (ADR-0074) and on the screen.
  - `log_level::DEBUG` (1) goes to the serial line only. It is never kept
    and never shown.
  - Any other level is `InvalidArgument`.

  Older programs passed 0, so they keep their meaning.
  `oceans_rt::debug_write_unkept` writes a DEBUG line.
- **The network service names each destination the first time** a socket
  connects to it (TCP) or sends to it (UDP, ping):

  ```text
  net: first contact udp 10.0.2.2:12123
  net: first contact tcp [2001:db8::1]:443
  net: first contact ping 10.0.2.2
  ```

  - Destinations are told apart by protocol, address and port. Ping has
    no port.
  - Accepted connections are not named: someone else reached us. For the
    same reason, a UDP socket answering an address and port it received
    from (the echo service answering a probe) is not named either.
  - Up to 256 destinations are remembered (`oceans_net::contacts`). Past
    that, one line says no more are named.
- **These lines are DEBUG,** because a list of where the network went is
  the user's history. A log that `diag save` copies to a USB stick must
  not hold it.

### The smoke check

Smoke boot 6 is a new computer's first boot after a factory reset, with
the usual services and the desktop. The check covers every destination
from power-on to switch-off. The boot is short: the clock asks before DHCP
has finished, and its 30-second retry (ADR-0143) usually comes after the
switch-off. The time servers are allowed either way.

- It may reach only:
  - the six test time servers it was given;
  - the host's HTTP server, which its script fetches from.
- Any other `first contact` fails the smoke test ("the network reached
  what no one asked for").
- An xtask test keeps the allowed list in step with the clock's servers in
  `services-smoke.conf`.

DHCP and IPv6 router and neighbour discovery are the network finding
itself. They are not sockets, so they are not named.

### How long a machine is supported (P8)

`docs/hardware/support-tiers.md` now says:
- a Tier 1 machine is supported for at least 5 years after it leaves the
  list;
- the CPU and firmware baseline is raised only by an ADR, 12 months ahead;
- nothing needs a TPM.

## What the user sees change

Nothing on the screen. The serial line shows one line for each new
destination. The kept log and `diag` hold none of them.

## Consequences

- Every new feature that reaches the network must either be asked for by
  the user, or be added to boot 6's allowed list by an ADR that says how
  the user turns it off. A forgotten "phone home" fails CI.
- WP-2 (Privacy & Permissions) can build on the same naming once the
  network service knows which app owns a socket. Today it does not: sockets
  are opened on an endpoint shared by all of its holders.
- The promises bind later work. Breaking one needs an ADR that says so and
  why.

## Alternatives considered

- **Keep the destination lines at INFO.** This is simpler and needs no ABI
  change. It lost because the kept log would become a browsing history on
  disk, which `diag save` copies anywhere.
- **Check every smoke boot.** The first boot exercises HTTPS, the AI
  gateway, DNS, IPv6 and the radio on purpose. An allowed list there would
  only repeat its script. Boot 6 is the one that stands for a user who has
  asked for nothing.
- **Name the process that owns each socket.** This is better, and it is
  needed for WP-2, but the network service cannot tell its clients apart
  yet. It is left to WP-2.
- **Write the promises only in the README.** That is not enforceable, and
  the README changes with every milestone. An ADR is the place decisions
  are kept.

## Checklist (master spec §48)

- **Purpose:** state what Oceans will never do to its user, and check the
  first of these in CI.
- **Architecture:**
  - a level in `DEBUG_WRITE` (kernel);
  - `oceans_net::contacts` (host-tested);
  - the network service's `contact()`;
  - xtask's `Contacts` per smoke boot.
- **API:** ABI 25 (`DEBUG_WRITE`'s `level`, `oceans_abi::log_level`);
  `oceans_rt::debug_write_unkept`.
- **Dependencies:** none new.
- **Security:**
  - destinations never reach the kept log or the screen;
  - writing a DEBUG line needs the same log capability as before;
  - an unknown level is refused, not taken as INFO.
- **Testing:**
  - `oceans-net` tests: each destination named once, the limit, the form
    of the lines;
  - xtask tests: the allowed list covers the clock's servers, and an
    unexpected contact is found;
  - smoke boot 6 in all three modes.
- **Failure behaviour:**
  - if the serial line drops a line, the check can only miss a
    destination, never fail a good boot;
  - past 256 destinations, naming stops and says so.
- **Promises touched:** P1 (kept by this ADR), P7 (template), P8
  (support window).
