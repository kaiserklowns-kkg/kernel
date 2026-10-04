# ADR-0049: App services

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0016 (init), ADR-0045 (Oceans Core), ADR-0046
  (packages), ADR-0047 (permissions)

## Context

Some installed software is not started by a person but runs in the
background:
- sync agents;
- monitors;
- the system services that the master spec says Go will power (§5).

init supervises the services in the boot image (ADR-0016), but
installed software must not be able to change init's configuration.
Oceans Core needs the same ability for packages, under the same
permission rules as apps.

## Decision

- **A package says what it is:** `kind = service` in its manifest
  (`kind = app` is the default). A service:
  - always runs detached;
  - has no terminal: instead of `console` it gets the **system log**
    (`log log` in its handle directory);
  - otherwise gets exactly what an app gets (ADR-0047).
- **`ENABLE` (op 12)** starts the service now and at every boot.
  - It needs every requested permission decided first: the client asks
    the user, as for a run.
  - Enabled services are kept in `/system/services`. Core starts them
    when it starts, once its endpoint can notice exits.
  - `ENABLE` of an app that is not a service is refused (`NotAService`).
- **`DISABLE` (op 13)** stops it and no longer starts it at boot.
  `REMOVE` disables too.
- **Restart policy:** an enabled service that exits with an error (not 0,
  not stopped by `STOP` or `DISABLE`) is restarted:
  - after 1 s, then 2, 4, 8, 16 s;
  - at most 5 times per boot, then Core gives up and records it in the
    audit log;
  - a timer bit on Core's notification drives the delays.

  Enabling again resets the count.
- **Rights:** both operations need `manage` (ADR-0048).
- **In the shell:** `app enable ID`, `app disable ID`. `app info` shows
  the kind and whether it is enabled.
- **The example:** `user/apps/heartbeat` (`app.oceans.heartbeat`)
  counts its starts in its storage. It fails on purpose the first time,
  to show the restart, and then keeps running.

### A fix found on the way

When `STOP` waits for an app to end, its slot is free, but the exit
signal for that slot is still to come. A `RUN` handled in between could
take the slot, and the late signal would then be read as the new app's
exit; Core would block waiting for it. Slots freed by `STOP` are now held
back until their signal has arrived.

## Consequences

- Installed software can provide background functions, supervised by
  Core with the same verification, permissions and audit as apps.
- **This is how Phase 6 will ship:** Go services are packaged and enabled
  this way.
- **Not done yet:**
  - **ordering between services**, and dependencies between them;
  - **a service providing an endpoint to others.** That needs Core to
    register and hand out service endpoints. It is planned when a second
    party needs to call an installed service.
  - **health checks beyond exit codes.**

## Alternatives considered

- **Adding installed services to init's `services.conf`:** the boot
  configuration would become writable by installation, and would bypass
  package verification and permissions.
- **Restarting forever:** a broken service would loop. Bounded backoff
  shows the failure (audit log) and stops wasting the system.

## Checklist (master spec §48)

- **Purpose:** background services from packages: start at boot, restart
  on failure.
- **Architecture:**
  - the manifest `kind`;
  - Core's enabled set, restart timer and backoff;
  - the shell commands.
- **API:**
  - `kind = service`;
  - `op::ENABLE`, `op::DISABLE`;
  - `field::KIND`;
  - `Status::NotAService`;
  - services get `log log`.
- **Dependencies:** none.
- **Security:**
  - services get only decided permissions;
  - enabling needs `manage` and the user's decisions;
  - nothing installed changes the boot image's services.
- **Testing:**
  - **Host:** manifest `kind` parsing.
  - **Smoke, boot 1:**
    - enabling an app is refused;
    - the service fails once, is restarted after 1 s, and runs.
  - **Smoke, boot 2:**
    - it starts at boot (third start);
    - it is disabled.
  - **Host check afterwards:** the audit log records enabling and
    disabling.
- **Failure behaviour:**
  - failed starts at boot are logged;
  - restarts are bounded;
  - giving up is audited.
