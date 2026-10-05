# ADR-0063: Trusting developers' keys

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0046 (packages and publisher signatures), ADR-0047
  (audit), ADR-0048 (narrower Core capabilities), ADR-0062 (the SDK)

## Context

Oceans Core installs only packages signed by a trusted publisher key
(ADR-0046). The trusted keys came from the boot image (`trust.keys`) and
nothing else, so no third-party developer could ever install an app
without rebuilding the system image.

Phase 8 needs apps from other developers, without loosening what makes a
package installable.

## Decision

- **The user can trust more keys, at the console only:**
  - `app trust add KEY PUBLISHER` (Core `TRUST`, which needs `manage`);
  - `app trust remove KEY`;
  - `app trust` lists all keys and where each came from (`TRUSTED`, which
    needs `query`).
- **Who can add keys:** `manage` is held by the user's agents (the shell,
  the desktop). It is never minted to apps, to the paired browser (query,
  run, audit, propose) or to AI sessions (query, run). Only the person at
  the console can add a key.
- **Added keys:**
  - are kept in `/system/trust.keys`, after the image's;
  - are loaded at boot;
  - are audited when added ("now trusts key … for publisher …") and when
    removed.
- **Refused:**
  - a key that is already trusted;
  - **a publisher name already trusted under another key**, so a developer
    cannot appear as "Oceans Examples" in dialogs and lists;
  - malformed lines.
- **The image's keys cannot be removed** from the console.
- **Removing a key:** packages are verified at every start (ADR-0046), so
  that developer's installed apps stop starting. The audit entry says so.
- **Installing and running are unchanged:** signature, publisher name,
  version and key continuity for updates, consent for permissions.

## Consequences

- Developers sign with their own keys (`oceans keygen`, ADR-0062). The
  user opts in once per developer, deliberately, at the console.
- A trusted key lets that developer's apps install. It grants them no
  permission: those are still decided per app.
- **A compromised developer key:** its packages install until the user
  removes the key. Revocation lists and expiring keys are later work.

## Alternatives considered

- **Developer mode (accept any signature):** this hides who is trusted,
  and it is easy to leave switched on.
- **Trusting keys from the Store or the browser:** remote parties would
  then decide who is trusted.
- **Trust prompted at install time ("trust this unknown key?"):** it
  trains users to click through. A deliberate console command is clearer
  for now. A desktop dialog may come with Settings.

## Checklist (master spec §48)

- **Purpose:** apps from developers other than the system's.
- **Architecture:** Core's trust list = the image's keys + the user's
  (`/system/trust.keys`).
- **API:** Core `TRUST` and `TRUSTED`; the shell's `app trust [add KEY
  PUBLISHER | remove KEY]`.
- **Dependencies:** none.
- **Security:**
  - console only (`manage`);
  - publisher names stay unique;
  - the image's keys are fixed;
  - removal takes effect at the next start;
  - everything is audited.
- **Testing:** smoke:
  - a third-party package is refused;
  - its key is trusted;
  - a second key under an existing publisher name is refused;
  - the list shows both sources;
  - the app installs and runs, and still runs after a reboot.
- **Failure behaviour:** a malformed or duplicate key, or a name already
  taken: refused with the reason; storage failure: the change is undone.
