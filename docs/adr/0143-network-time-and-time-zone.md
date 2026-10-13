# ADR-0143: Network time, and the time zone

- Status: Accepted
- Date: 2026-10-14
- Depends on: ADR-0031 (the wall clock from the real-time clock),
  ADR-0085 (the system information object's rights), ADR-0136 (the
  machine's settings in Core)
- Part of Phase 10 (Alpha). ABI 24.

## Context

On the first real machine the clock was 7 hours behind:

- The wall clock is the real-time clock (RTC) read once at boot, taken
  as UTC, and shown as UTC.
- There was no time zone (Thailand is UTC+7).
- Nothing corrected an RTC that drifts, or that another system kept in
  local time.

## Decision

- **`CLOCK_SET` (system call 51, ABI 24):** `(system, unix_ms)`.
  - It needs `WRITE` on the system information object. `MANAGE` (power,
    ADR-0085) stays init's alone, and `grant = clock` hands out
    READ|WRITE.
  - It accepts times from 2000 to 2200.
  - The kernel moves its wall clock and writes the RTC, in the chip's
    encoding (BCD or binary, 12- or 24-hour). The century register is
    written only if it already holds a century.
  - It logs the change and who made it.
- **`oceans-ntp`** (host-tested): SNTP requests and replies.
  - The request's transmit timestamp is a random nonce (RFC 9109), which
    the reply must echo.
  - Replies are refused when they are not a server's answer, not ours,
    not synchronized (leap 3, stratum 0 or above 15), or have zero or
    out-of-order times.
  - Era 1 (2036 on) is handled.
  - The time at arrival is the server's send time plus half the round
    trip, less the server's own time.
- **`net-proto::time_via`:** one server asked, three tries, over UDP like
  DNS.
- **The `clock` service** (`grant = log`, `grant = clock`, `use = net`):
  - It asks `0`–`3.pool.ntp.org`, or the servers in its arguments.
  - It sets the clock only when two different servers agree within 2 s.
    One server, or one pretending on the local network, cannot move it.
  - It skips changes under 0.5 s.
  - It retries every 30 s until it has set the clock, then every 6 hours.
- **`args = WORDS`** in init's configuration: a service's arguments,
  as the shell gives a program's (`Directory::args`).
- **The time zone:**
  - Core ops `TIME_ZONE` and `SET_TIME_ZONE` (query and decide) hold
    minutes ahead of UTC in `/system/timezone`. It is the machine's
    setting, like the scale, and UTC until chosen.
  - `TIME_ZONES` lists 24 offsets, each with the places that keep it.
  - The desktop's clock and the notifications panel show local time.
  - Settings ▸ General shows the local date and time, the zone, and
    West/East to change it.

## Consequences

- The ZBook's clock is right once it is online; after Bangkok is chosen
  in Settings, it shows Thai time. The RTC keeps it for the next boot,
  online or not.
- The smoke test:
  - The host runs two SNTP servers 90 s ahead, on two free ports of
    12123–12128, since other smoke runs may hold some.
  - The smoke configuration's clock service asks them, and the test
    expects `clock: set from network time` and the kernel's log of
    `init/clock` setting the clock.
  - Host tests cover the packets.
- Trust: SNTP is not authenticated. Two agreeing servers and a random
  nonce stop a single liar and blind spoofing, not someone controlling
  the whole path. Certificates (TLS) depend on the clock, so this matters;
  NTS (RFC 8915) is the lasting answer.
- Not yet:
  - daylight saving time (fixed offsets: London and New York are an hour
    off in summer);
  - the shell's `date`, which still says UTC;
  - a zone chosen at setup.

## Alternatives considered

- **Keeping local time in the RTC (as Windows does):** every reader
  would need the zone, and a machine shared with Linux would disagree.
- **SNTP inside the kernel:** the network is a service; the kernel only
  takes the result.

## Checklist (master spec §48)

- Purpose: the right time, shown where the user is.
- Architecture: a system call, a library, a service, a Core setting.
- API: `CLOCK_SET`; Core's `TIME_ZONE` and `SET_TIME_ZONE`.
- Dependencies: none.
- Security: the clock right is granted only to the clock service; two
  servers must agree; replies are checked.
- Testing: host tests, the smoke test against host servers.
- Failure: no answer leaves the RTC's time, and retries.
