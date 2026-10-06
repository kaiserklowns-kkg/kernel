# ADR-0083: Activity Monitor

- Status: Accepted
- Date: 2026-10-06
- Depends on: ADR-0080 (the toolkit, apps that come with the system),
  ADR-0081 (system apps are not asked), `sysinfo` (the process list and
  memory, as `ps` and `free` read them)
- Part of Phase 10 (Alpha: basic apps).

## Context

What runs, and how much memory it takes, could be seen only in the shell
(`ps`, `free`). A desktop needs it in a window, kept current.

## Decision

Activity Monitor is an app on the toolkit, brought by the image. Its
permissions are `window` and `system-info` (read-only: the process list
and the memory).

- **What it shows:**
  - three cards: memory used of total (with a bar, red above 90 %), the
    processes running, and how long the system has been up;
  - the processes, largest memory first: PID, name, memory, and state
    (running, or the exit code).
- **Kept current:** the toolkit gains `run_ticking`, which is `run` with a
  frame every `tick_ms` as well as on input (a timer on the app's
  notification). Activity Monitor reads again every second; `run` is
  `run_ticking` without a tick.

## Consequences

- The processes and the memory can be watched without the shell.
- Apps that change by themselves (a clock, progress) have a toolkit way to
  redraw.
- **Not yet:**
  - ending a process from the window (it needs a right to stop other
    processes, its own decision);
  - CPU time per process (the kernel does not count it yet);
  - more than the rows that fit are not scrolled to.

## Alternatives considered

- **Redrawing only on input:** the numbers would go stale while the window
  is left open.
- **A thread that wakes the app:** a timer on the notification it already
  waits on is simpler and needs no thread.

## Checklist (master spec §48)

- **Purpose:** processes and memory in a window.
- **Architecture:** `user/apps/activity` on `oceans-ui`; `run_ticking` in
  `oceans-ui`.
- **API:** `oceans_ui::run_ticking`.
- **Dependencies:** none.
- **Security:** `system-info` is read-only; nothing is changed.
- **Testing:** smoke: Activity Monitor installed with the image, started,
  and its window (a card) on screen.
- **Failure behaviour:** without `system-info`, it says the information is
  not available.
