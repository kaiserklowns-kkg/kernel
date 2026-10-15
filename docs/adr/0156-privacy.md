# ADR-0156: Privacy in Settings, and the microphone in the top bar

- Status: Accepted
- Date: 2026-10-15
- Depends on: ADR-0047 (permissions), ADR-0081 (Settings), ADR-0087
  (recording), ADR-0096 (reader ends), ADR-0146 (the promises)
- Part of Phase 10 (Alpha). No ABI change.
- Plan: WP-2 of [development/plan-lessons-from-other-systems.md](../development/plan-lessons-from-other-systems.md).

## Context

Promise P5 (ADR-0146): every permission is visible and can be revoked, and
the user is asked once, not nagged.

- **Windows** spreads permissions over many pages and some apps' own
  settings.
- **macOS** gathers them in Privacy & Security, but asks again every
  month for screen recording.

Oceans asks once (ADR-0047), and Settings → Apps shows one app's
permissions at a time. Nothing answered the question people actually ask:
"which apps can use the network (or my files), and did they?". Nothing on
the screen said that the microphone was recording.

## Decision

### Core remembers when each permission was last held

- When an app starts holding a permission the user decided (not an
  automatic one), Core notes the time.
- The times are kept in `/system/permissions-used` as `APP PERMISSION
  SECONDS` lines (`oceans_package::privacy::Used`, host-tested).
- Removing an app forgets its times.
- A damaged line is skipped. Only a time shown is lost, never a decision.
- **"Last used" means "last started with it".** Services cannot yet tell
  which app uses a socket or a file: a network end is shared by all its
  holders (ADR-0146). The page says "last used" for the start, which is
  when the app could use it.

### Two Core ops

| Op | Right | Data → reply |
|---|---|---|
| `PRIVACY` (51) | `AUDIT` | `[index u32]` → one line `PERMISSION\tDECISION\tSECONDS\tAPP` (`privacy::Entry`); `NotFound` past the last |
| `IN_USE` (52) | `QUERY` | → `[flags]`: bit 0 set while the microphone records |

- `PRIVACY` lists every installed app's every non-automatic permission,
  one entry per call, because a reply holds 256 bytes.
- It needs `AUDIT`, as what apps were allowed to do is the user's own
  record. An app's ordinary Core end (`QUERY`) cannot read it.
- `IN_USE` says nothing about who: the desktop needs only whether.

### The audio service says whether it records

`oceans_audio_proto::op::RECORDING` (13) → `[0 | 1]`, 1 while the input runs
for a capture session.
- Any end may ask, except a player's: an app that can only play learns
  nothing about the input (ADR-0094).
- `oceans_audio_proto::recording` is the client.

### Settings → Privacy

A new section between Apps and Users.
- For each permission that is asked of the user (Network, Files, Pointer,
  Notifications, …), with its description, the section lists each app that
  asks for it, with:
  - its decision;
  - "last used YYYY-MM-DD HH:MM" in the user's time zone, or "never used";
  - Allow, Deny and Ask, with the one in force filled.
- A decision goes through `DECIDE` as made in Settings, and is audited as
  in Apps.
- Denying a permission an app holds stops that app (ADR-0047).

### The microphone dot

- The desktop asks `IN_USE` every 250 ms. While the microphone records, an
  orange dot sits left of the keyboard layout's name in the top bar.
- It is drawn by the desktop, never by an app, so an app can neither hide
  it nor fake it.
- The desktop logs `desktop: the microphone is in use` and `… is no longer
  in use`.

### The shell

`app privacy` prints `  APP PERMISSION DECISION, used | never used` for each
entry.

## What the user sees change

- **Settings:** a Privacy section, and the sections after it move down one.
- **The top bar:** an orange dot while anything records.

## Consequences

- One place answers "what can apps do here, and did they".
- Only the shell's `play record` can record today, as apps have no
  microphone permission yet. When one comes, the dot is already there.
- "Last used" becomes exact when services can name the app behind a
  socket or a file. That is the next step for this page, and it is what
  ADR-0146 left open.
- The AI activity log (ADR-0051) is not on this page yet. It lives in the
  AI service, which Settings has no end to.

## Alternatives considered

- **One reply with every line.** A Core reply holds 256 bytes, and a few
  apps fill it. One entry per call, as `AUDIT` and `TRUSTED` do, has no
  limit.
- **Push recording changes to the desktop.** That needs a notification
  from the audio service through Core. Polling a one-byte answer four
  times a second costs nothing measurable, and it cannot miss a change it
  was not subscribed to.
- **Let `QUERY` read `PRIVACY`.** Any app with a Core end could then learn
  which other apps the user trusts with what.

## Checklist (master spec §48)

- **Purpose:** permissions visible by kind and by use, and the microphone
  never in use unseen.
- **Architecture:**
  - `oceans_package::privacy` (the file and the line);
  - Core's `used`, `PRIVACY` and `IN_USE`;
  - the audio service's `RECORDING`;
  - the desktop's dot;
  - Settings' Privacy;
  - the shell's `app privacy`.
- **API:** Core ops 51 and 52; audio op 13. No kernel ABI change.
- **Dependencies:** none.
- **Security:**
  - `PRIVACY` needs `AUDIT`;
  - `RECORDING` is refused to players;
  - the dot is drawn by the system;
  - decisions go through the existing, audited `DECIDE`.
- **Testing:**
  - `oceans_package::privacy` tests: times kept and read back, damaged
    lines skipped, removed apps forgotten, lines that round-trip, wrong
    lines refused;
  - smoke: `app privacy` after the network is granted ("allowed, used")
    and after it is revoked ("denied, used");
  - the desktop's log while `play record` records.
- **Failure behaviour:**
  - no audio device, or Core cannot ask it: not recording, so no dot;
  - no clock: no time is noted;
  - an unreadable file: no times, decisions untouched.
- **Promises touched:** P5 (kept), P4 (the microphone cannot be used
  unseen), P7 (a section added).
