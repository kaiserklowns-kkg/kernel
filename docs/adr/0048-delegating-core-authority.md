# ADR-0048: Delegating Oceans Core authority

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0011 (capabilities), ADR-0045 (Oceans Core)

## Context

The `core` capability (ADR-0045) was all or nothing. Its holder could:
- install anything;
- run and stop any app;
- answer consent;
- grant and revoke permissions.

That is right for the user's agent (the shell), but other programs need
less:
- a script that only lists apps;
- a launcher that may start them;
- above all the AI runtime (Phase 6), whose agent sessions may start apps
  but must never answer their own consent (ADR-0007).

Capability systems delegate by **attenuation**: handing someone a weaker
version of what you hold.

## Decision

- **Rights.** A Core client end carries a set of rights
  (`oceans_core_proto::access`):

  | Right | Allows |
  |---|---|
  | `query` | `LIST`, `INFO`, `PERMISSION` |
  | `run` | `RUN`, `STOP` |
  | `manage` | `INSTALL`, `REMOVE`, `ROLLBACK`, `ENABLE`, `DISABLE` |
  | `decide` | `DECIDE`: answering consent, granting and revoking |
  | `audit` | `AUDIT` |

  The unbadged end init hands out has all of them.
- **`MINT` (op 11)** returns a new client end, badged by Core, with the
  rights asked for. They **must be among the caller's own**: a holder can
  only narrow, never widen (`Denied` otherwise). Minted ends can mint
  further, narrower still.
- **Enforcement.** Core checks every request against the rights of the
  end it came through. A missing right answers `Denied` ("not allowed by
  this capability"), and any capability sent along is closed. When the
  last handle to a minted end closes, Core forgets its badge.
- **In the shell:** `run PROGRAM core:RIGHTS` mints a Core end with those
  rights for that program only, e.g. `run apps out core:query+run -- start
  ID`. **`apps`** is a utility that does what its capability allows: list,
  start, stop, mint.
- **Consent stays with `decide`.** A `run`-only holder starting an app
  whose permissions are undecided gets `NeedsConsent`, and cannot answer
  it. Only an agent holding `decide` (the user's) can.

## Consequences

- Programs and agents get exactly the app authority they need, and that
  authority ends with their handles.
- **The AI runtime (ADR-0051)** receives, per session, a Core end the
  user's agent minted for it, without `decide`.
- **What attenuation does not do:** minted ends are not revocable
  separately from closing them. A holder that leaks its end leaks those
  rights until the end is closed, as with any capability. Revocable minted
  ends can be added if a use needs them.

## Alternatives considered

- **Separate endpoints per right:** Core can receive on only one, and
  init would have to hand out each.
- **Rights checked by the client:** no protection at all; checks belong
  in the server.
- **Identity-based rules (which program is calling):** Oceans has no
  ambient identities by design (ADR-0006). What you hold is what you may
  do.

## Checklist (master spec §48)

- **Purpose:** handing out narrower Core authority.
- **Architecture:** badges with rights in Core; `MINT`; checks per
  request.
- **API:**
  - `access` rights and `access::parse`;
  - `op::MINT`;
  - `Status::Denied`;
  - the shell grant `core:RIGHTS`;
  - the `apps` utility.
- **Dependencies:** none.
- **Security:**
  - attenuation only;
  - every request checked server-side;
  - minted ends forgotten when closed;
  - `decide` never implied.
- **Testing (smoke):**
  - `apps` with `core:query` lists, and is refused a start;
  - with `core:query+run` it starts and stops an app, can mint `query`,
    and is refused minting `decide`.
- **Failure behaviour:** refusals are `Denied` with nothing done; an
  unknown right name is a usage error.
