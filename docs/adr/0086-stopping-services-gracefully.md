# ADR-0086: Stopping services gracefully

- Status: Accepted
- Date: 2026-10-07
- Depends on: ADR-0085 (switching off), ADR-0016 (init), ADR-0040
  (NVMe), ADR-0069 (AHCI), ADR-0074 (the kept log), ADR-0045 (Core)
- Adds: `grant = stop`, `stop-timeout = MS`, `oceans_rt::STOP`
- Part of Phase 10 (Alpha).

## Context

ADR-0085 stops the system by killing every service. The volume stays
consistent, but four things are lost:
- **The SSD** is never told: without `CC.SHN` it records an unsafe
  shutdown each time. The driver already shuts the controller down when
  its loop ends, but a kill never lets it get there.
- **A SATA disk** loses power with its heads loaded (an emergency
  retract) and its cache possibly unflushed.
- **The kept log** misses its last couple of seconds, which are the
  shutdown's own lines.
- **Apps run by Core** survive Core's death as orphans (ADR-0044). They
  write after the disks were synced, and those writes are lost.

## Decision

- **`grant = stop`:**
  - init creates a fresh notification for each run of the service and
    hands it over with SIGNAL, WAIT, DUPLICATE and TRANSFER. init keeps a
    SIGNAL-only end.
  - The service may make it its main notification: bind it to its
    endpoint, set timers on it, watch processes with it.
  - Bit `oceans_rt::STOP` (1 << 62) is init's alone. It means: finish
    what is under way, leave things consistent, exit.
- **Stopping** (in init's `stop_system`, same order as ADR-0085):
  - A service with a stop notification gets `STOP`. It then has
    `stop-timeout` (default 5000 ms) to exit, and only then is it
    killed.
  - init waits on its own event notification with a timer bit (63).
    Exits seen meanwhile are remembered, and a power request that comes
    in now is answered `NOT_POSSIBLE`.
  - Services without the grant are killed, as before.
- **Who asks to be told:**
  - **nvme:** the notification is bound to its endpoint. On `STOP` it
    leaves its loop and runs its existing shutdown (`CC.SHN` normal,
    waiting for `CSTS.SHST` complete), logging "the controller is shut
    down".
  - **ahci:** the same. Its shutdown flushes the cache, then sends the
    new **`STANDBY IMMEDIATE`** (0xE0, `oceans-ahci`) and stops the port.
  - **logkeep:** it waits on the notification with a 2 s timer instead of
    sleeping. On `STOP` it copies and syncs once more, so the kept log
    ends with the shutdown.
  - **Core:** it uses the notification as its own: apps' exit bits, the
    restart timer at bit 63. On `STOP` it stops every running app and
    service, then exits. As Core starts after `fs`, apps' files are
    closed, and so committed, before init syncs the disks.
- No ABI change: notifications, timers and binding already do this.

## Consequences

- SSDs and SATA disks are shut down as their makers ask, and Core's apps
  no longer outlive the sync.
- `diag previous` after a clean shutdown ends with it. The smoke test
  checks this: the second boot finds "asked to restart" in the first
  boot's kept log.
- A shutdown takes longer: each service with the grant gets up to its
  timeout. A hung one costs 5 s, then it is killed as before.
- **Limits:**
  - Programs the shell runs are still orphans, and so are a service's own
    children unless it stops them. The shell holds no files open between
    commands.
  - The display, the network stack and the AI services are still killed:
    they hold nothing that must reach a disk.
  - The USB mass-storage driver is not asked yet. USB sticks have no
    standby protocol beyond the `SYNC` already made.

## Alternatives considered

- **A reserved `STOP` IPC label on every service's endpoint.** init
  already holds client ends. But an IPC call has no timeout, so a hung
  service would hang the shutdown. Services without an endpoint (logkeep)
  could not be reached either.
- **A kernel "terminate" signal on processes.** It would need a new
  delivery mechanism in the kernel and a way for each runtime to handle
  it. A notification is already exactly that, and needs nothing new.
- **Stopping everything in parallel.** Faster, but the order matters
  (clients before `fs`, `fs` before the disks), and the slow ones (an SSD
  shutdown) are at the end anyway.

## Checklist (master spec §48)

- **Purpose:** let services finish before the power goes. Clean disk
  shutdowns, a complete kept log, no orphaned apps.
- **Architecture:** init asks and waits, then kills. Each service decides
  what stopping means for it.
- **API:** `grant = stop`, `stop-timeout = MS`, `oceans_rt::STOP`;
  `AtaCommand::standby_immediate`.
- **Dependencies:** none new.
- **Security:**
  - the service's end can wait on and signal its own notification only;
  - another service cannot ask it to stop, because init alone holds an
    end of it;
  - the timeout bounds what a service can delay.
- **Testing:**
  - unit test of the new ATA command;
  - smoke:
    - Core's apps stopped, core, logkeep and nvme reported stopped;
    - the SSD shut down;
    - the next boot's `diag previous` shows the restart;
  - `smoke-hw`: NVMe shutdown and SATA standby before the real ACPI reset
    and S5.
- **Failure behaviour:**
  - a service that does not stop in time is killed, with a log line;
  - a disk that refuses standby is logged, and the shutdown continues;
  - an NVMe controller that never reports shutdown complete is logged
    (as before), and the power goes anyway.
