# ADR-0047: Permissions and consent

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0006 (capability security), ADR-0007 (AI mediation),
  ADR-0045 (Oceans Core), ADR-0046 (packages)
- Completes the permission broker ADR-0006 announced.

## Context

The master spec says apps get nothing automatically: files, network,
devices and the rest need the user's approval, in terms a normal user
understands (§23, §36). ADR-0006 put this policy in a userspace
permission broker granting capabilities, and ADR-0007 added a rule for
everything later built on it, AI included: **the system, not the
requester, describes what is being asked**.

Oceans' capabilities make enforcement simple. A process can only use the
handles it holds. The question is which handles an app gets, who decides,
how the decision is kept, and how it is taken back.

## Decision

### The catalog (`oceans_package::Permission`)

| Permission | Grants | Without asking |
|---|---|---|
| `console` | output to the terminal that started the app | yes |
| `storage` | the app's own data directory | yes |
| `system-info` | read-only processes, memory, uptime | yes |
| `network` | the network service | no |
| `files` | the user's files (`/home`) | no |
| `pointer` | mouse and tablet events | no |

- **"Without asking"** is the low-risk rule of ADR-0020: it reaches only
  the app itself or read-only information.
- **The list grows with the system:** notifications, camera, microphone,
  location and AI arrive with their services. A manifest naming an
  unknown permission is refused, never half-granted.

### Rules

- **An app gets only what its manifest asks for.** Of that it gets the
  automatic permissions plus those the user **allowed**. Denied ones are
  absent: the handle is simply not in its directory, and apps are
  expected to work without (the example prints "no network permission").
- **When it is asked:**
  - **First run:** `RUN` answers `NeedsConsent` while any requested
    permission is undecided. The client asks the user about each, sends
    `DECIDE` (allow or deny, from a prompt), and runs again.
  - **Later:** a decision stands until changed. `app grant` and
    `app revoke` change it by command.
- **Revocation:** a capability cannot be taken back from a process, so
  revoking a permission that a running app holds **stops the app**
  (ADR-0044). Its next run is without it.
- **Updates:** a version that asks for something new is asked at its
  next run. Decisions about what it no longer asks for are dropped.
  Removing an app removes its decisions.
- **Persistence:** decisions are kept in `/system/permissions`, one per
  line: `ID PERMISSION allow|deny`.
- **Audit:** every decision is recorded with its time, the app, the
  permission and how it was made (at its prompt, or by command), along
  with each start and the permissions it got. The record is
  `/system/audit.log` and the kernel log; `app audit` shows the recent
  ones.

### Consent is rendered by the system

The consent agent is today the shell, later the system UI's permission
dialog. It shows:
- the app's name, id, version and **verified** publisher;
- the permission's meaning, **in the system's words** from the catalog;
- the app's declared reason, as a quotation marked as the app's.

An app cannot word the question, cannot answer it, and holds no
capability to `core`. The answer is yes only for an explicit `y`; the
default is no.

## Consequences

- Least privilege is the default. An app's power is its manifest
  intersected with the user's decisions, visible with `app info` and
  auditable afterwards.
- **For the AI runtime (Phase 6):** agents get the same mediation, per
  tool and per action (ADR-0007). The catalog and the consent flow are
  where that plugs in.
- **Not in this version:**
  - **"Allow once" and time-limited grants:** they need Core to grant per
    run; to be added with the UI.
  - **Permissions requested while an app runs**, rather than at start.
  - **Narrower scopes**, such as one folder or one host.
  - **Per-user decisions:** there is one user for now.

## Alternatives considered

- **Install-time consent (all or nothing):** users approve without
  reading, and a later need cannot be refused separately. Asking per
  permission when first needed, and remembering, is clearer.
- **Taking capabilities back from running processes (revocable
  handles):** the kernel has revokers (ADR-0011), but every service would
  need to cope with handles dying under live sessions. Stopping the app is
  simple and certain. Revocable delegation can come later, where a
  service is ready for it.
- **Letting apps supply the prompt text:** an app could misdescribe what
  it wants; only the reason is the app's, and it is shown as a quote.

## Checklist (master spec §48)

- **Purpose:** user-controlled permissions for apps.
- **Architecture:**
  - the catalog in `oceans-package`;
  - decisions, enforcement and audit in Core;
  - prompts in the consent agent (the shell).
- **API:**
  - `PERMISSION`, `DECIDE` (with its source), `AUDIT`;
  - `NeedsConsent`;
  - the permission names.
- **Dependencies:** none.
- **Security:**
  - deny by default;
  - capabilities granted only per decision;
  - revocation enforced by stopping the app;
  - the system words the prompts;
  - everything is audited;
  - apps hold no capability to Core.
- **Testing:**
  - **Host:** catalog round trip, and which permissions are automatic.
  - **Smoke:**
    - the prompt shows the system's text and the quoted reason;
    - a "no" leaves the app without network (it says so);
    - `app grant` gives it network (it reaches the host);
    - `app revoke` while it runs stops it;
    - decisions and the audit trail persist across a reboot;
    - the host checks the audit log for every step.
- **Failure behaviour:**
  - undecided means not granted;
  - an unreadable answer means no;
  - a decision that cannot be stored is reported and not applied;
  - a permission the system cannot provide at a start (e.g. no network
    service) is logged, and the app runs without it.
