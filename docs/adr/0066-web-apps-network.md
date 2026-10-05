# ADR-0066: The network for web apps

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0047 (consent), ADR-0064 (web apps)

## Context

A web app's page could reach only its own origin, the bridge's app port
(`connect-src 'self'`). Many apps need servers of their own (an API, a
sync service).

A web app runs in the paired browser. Reaching another server is the
browser's network, but it is still the app sending data out, and the user
should decide that.

## Decision

- **Web apps may ask for `network`**, besides `storage`. Nothing else is
  accepted in their manifest.
- **How the user decides:** `network` is not automatic, and web apps never
  `RUN`, so no run asks. The user decides with `app grant ID network` at
  the console, or in Settings later. The Store's install dialog
  (ADR-0061) shows that the app asks for it.
- **The effect:** when the bridge serves the page and the decision is
  allowed, the page's policy adds **`https:`** to `connect-src`. Until
  then it reaches only its own origin. Revoking takes effect at the page's
  next load.
- **Only https:** no plain http, and no other kinds of source: images,
  scripts and styles stay the app's own.

## Consequences

- Web apps can talk to https services once allowed. The sandbox (ADR-0064)
  still keeps them out of the system's pages and other apps.
- Servers must allow requests from a sandboxed page (CORS with an opaque
  origin), which public APIs usually do.

## Alternatives considered

- **Allowing the network to all web apps:** that would let them send data
  out unasked.
- **A proxy in the bridge:** this puts the network on Oceans and the
  bridge in the middle of every request, for nothing the browser cannot do
  under a policy.

## Checklist (master spec §48)

- **Purpose:** the network for web apps.
- **Architecture:** the manifest rule; the bridge's page policy follows
  the decision.
- **API:** `network` in a web app's manifest; `connect-src … https:`.
- **Dependencies:** none.
- **Security:** consent; https only; connections only (no scripts from
  elsewhere); the sandbox unchanged.
- **Testing:** unit: the manifest rule (storage and network, not files);
  the bridge's policy without and with the decision; the policy rewrite.
- **Failure behaviour:** undecided or denied: the strict policy.
