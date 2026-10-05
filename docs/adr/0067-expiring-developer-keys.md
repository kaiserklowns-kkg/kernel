# ADR-0067: Developer keys that expire

- Status: Accepted
- Date: 2026-10-05
- Depends on: ADR-0031 (wall-clock time), ADR-0063 (trusting developers'
  keys)

## Context

A trusted developer key (ADR-0063) was trusted until the user removed it.
A lost or stolen key would keep its power for as long as nobody noticed.

Trust that is meant to be temporary should end by itself: a contractor's
key, a test build's.

## Decision

- **Trust through a date:**
  - `app trust add KEY PUBLISHER --until YYYY-MM-DD` trusts the key
    through that day (UTC);
  - the trust file's line becomes `KEY until=YYYY-MM-DD PUBLISHER`
    (`oceans_package::trust_entries`, `date`);
  - a date already past is refused.
- **Checked whenever trust is used:**
  - Core builds the trusted keys at every install and every start, so a
    key past its day trusts nothing: its apps no longer install or start;
  - **unknown time fails closed:** without a wall clock, a dated key is
    not trusted;
  - keys without a date behave as before.
- **The image's keys are never dated:** a dated line in the boot image's
  list is not trusted (`trusted_keys`).
- **Visible:**
  - `app trust` shows "until DATE" or "expired after DATE";
  - the audit says the date;
  - `oceans trust --key FILE --until DATE` prints the command.
- **Removing a key** (`app trust remove`) stays the way to revoke at once.

## Consequences

- Temporary trust ends without anyone remembering to end it.
- A machine whose clock is wrong may trust a dated key too long or too
  short. The clock comes from the firmware (ADR-0031), and network time is
  later work.
- Revocation lists published by developers are not part of this. Removal
  at the console and expiry cover the local cases.

## Alternatives considered

- **Expiry inside the key (a certificate):** this needs a format and a
  signer above developers, which Oceans does not have.
- **Trusting dated keys when the time is unknown:** that fails open,
  against the master spec.

## Checklist (master spec §48)

- **Purpose:** trust that ends by itself.
- **Architecture:** dated trust entries; Core filters at every use.
- **API:** `TRUST` with `until=`; the `TRUSTED` flag "expired"; the shell's
  `--until`; `oceans trust --until`; `oceans_package::{TrustEntry,
  trust_entries, date}`.
- **Dependencies:** none.
- **Security:** fails closed; the image's keys are fixed; audited.
- **Testing:**
  - unit: dates (round trip over decades, leap days, bad dates), dated
    lines (valid through the day, not after, not without time, not in the
    image's list);
  - smoke: the developer's key is trusted until 2099-12-31 and its apps
    run (after a reboot too); a key dated 2020-01-01 is refused; the list
    shows the date.
- **Failure behaviour:**
  - a bad or past date: refused;
  - an expired key: its apps are refused as untrusted.
