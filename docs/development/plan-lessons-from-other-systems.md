# Development plan: fixing what macOS, Windows and Linux get wrong

Status: plan, 2026-10-14. Built from the research in
[architecture/lessons-from-macos-windows-linux.md](../architecture/lessons-from-macos-windows-linux.md).

Each work package (WP) below is a milestone done the usual way: an ADR with
the master spec §48 checklist, tests, `cargo xtask check`, the smoke modes,
then commit, push and CI. This plan sets **what** each WP must achieve and
**how it is accepted**. Its ADR settles the details: op numbers, ABI and
file formats.

## 1. The promises

These are the rules every WP serves. WP-1 writes them into an ADR, and from
then on every ADR's checklist says which promise it touches.

| # | Promise | Fixes | Guarded by |
|---|---|---|---|
| P1 | **No telemetry, no ads.** The system connects to the network on its own only for what the user can see and turn off: network time (ADR-0143) and update checks (WP-4) | Windows' required diagnostic data, ads in the Start menu and bundled apps | WP-1's smoke check of the connections an idle system makes |
| P2 | **No online account is needed.** Every account is local | Windows' forced Microsoft account | Setup and Settings have no sign-up path (review) |
| P3 | **The machine is the user's.** The system never restarts, updates or reinstalls anything without the user. Defaults the user chose stay chosen | Windows' forced restarts and Edge taking defaults back | WP-4's tests; WP-8's defaults test |
| P4 | **AI never watches.** AI has no screen capture and no background indexing. Every read is approved, and every action is logged (ADR-0007, 0055) | Recall | A review rule: no AI tool reads display memory. WP-2 shows the log |
| P5 | **Every permission is visible and revocable,** and the user is asked once, not nagged | Windows hides permissions in many places; macOS asks again every month | WP-2 |
| P6 | **A failure is contained and undone.** A driver crash is not a system crash. An update can always go back | CrowdStrike 2024, bricked updates | Userspace drivers (ADR-0021), slots (ADR-0071), safe mode (ADR-0122), WP-5 |
| P7 | **Nothing moves without a reason.** A visible change to where something is or how it works is written in its ADR | The churn of Windows 11 and macOS Tahoe | WP-9, an ADR template section |
| P8 | **Know before you install.** The user learns what works on the machine before anything is erased. Each hardware tier has a stated support window | Linux on the wrong laptop; hardware cut off by TPM 2.0 and Apple silicon | WP-3, WP-10 |
| P9 | **Anyone can check a release.** Builds are reproducible, and dependencies are few and audited (ADR-0004) | The xz backdoor | WP-6 |
| P10 | **Legibility before effects** | Liquid Glass | WP-7 |

## 2. Where things stand

What exists today, so no WP rebuilds it:

- **Permissions:** asked at first run. Settings shows each app's
  permissions, with allow, deny or ask again (`decision::FORGET`)
  (`user/apps/settings`, ADR-0047, 0081). **Missing:** a view by
  permission, when each was last used, and indicators while a sensitive
  device is in use.
- **Service restarts:** `restart = always | on-failure | never`, with
  backoff and `max-restarts` (ADR-0016). **Missing:** clients that find
  their provider again after it restarts, and telling the user when a
  service gives up.
- **Hardware table:** `libs/hardware` drives `sysreport` and
  `docs/hardware/compatibility.md` (ADR-0068). **Missing:** using it
  *before* installing, in OceansSetup.
- **Updates:** `run update … apply FILE` from the shell writes to the
  other slot (ADR-0071, 0092). **Missing:** finding updates over the
  network, and doing it from Settings.
- **Logs and diagnostics:** kept on the machine (ADR-0070, 0074). Nothing
  is sent anywhere.
- **Uninstall:** `app remove ID [--keep-data]` (ADR-0045). **Missing:**
  "reset app" in Settings, and a test that nothing is left behind.

## 3. Work packages

Sizes: **S** is a day or less, **M** is a few days, **L** is a week or more.

### WP-1 — The promises ADR, and the idle-network check (S)

- **Goal:** P1–P10 written as a single ADR, and a smoke check for P1.
- **Design:**
  - The `net` service names each destination once, as `net: first contact
    udp 10.0.2.2:123` (an address, never the data). Which process opened
    the socket is not known yet; that is left to WP-2.
  - Smoke boot 6 (a new computer's first boot) fails if it reaches
    anything other than its time servers and its scripted fetch.
- **Security:** the lines go to the serial line only (ABI 25's DEBUG
  level). They are never kept on disk, since they would be the user's
  history.
- **Done:** ADR-0146.
- **Accepted when:** the ADR is merged, the check runs in `cargo xtask
  smoke`, and a deliberately added test connection makes it fail.

### WP-2 — Privacy & Permissions in Settings (M)

- **Goal (P5, P4):** one page that answers "who can do what, and when did
  they last".
- **Design:**
  - **Core** keeps, for each app and permission, the time the app last
    started holding that grant. A new `query` op returns it.
  - **Services that can tell real use** report it to Core: `net` on the
    app's first connection, `audio` while recording.
  - **The page:**
    - one row per permission (Network, Files, Pointer, Microphone, …);
    - under each, the apps allowed, with "last used";
    - Deny or Ask again on each, through the same `DECIDE` path as today.
  - **The menu bar** shows a dot while any app records sound
    (ADR-0087). The dot is drawn by the desktop, never by the app.
  - **AI activity:** the AI log (ADR-0051) is shown on the same page, newest
    first.
- **Security:** reading the times needs `query`, and changing a decision
  needs `decide`, as now. Apps cannot see other apps' grants.
- **Failure:** if a service never reports use, the page shows "last
  started with" instead of "last used". It does not show nothing.
- **Tests:**
  - Core: host tests for the times;
  - Settings: a model test that every catalog permission gets a row;
  - smoke: run `hello` with network, then check the time is set.
- **Accepted when:** revoking Network from the page closes the app's
  network on its next start, and the recording dot appears in the
  smoke screenshot while the sound test records.

### WP-3 — Know before you install: the hardware check in OceansSetup (M)

- **Goal (P8):** the Linux lesson. Most bad experiences come from the
  wrong machine, so tell the user before erasing anything.
- **Design:**
  - `tools/setup` and `OceansSetup-linux` list PCI functions:
    - Windows: SetupAPI hardware ids `PCI\VEN_xxxx&DEV_xxxx`;
    - Linux: `/sys/bus/pci/devices/*/{vendor,device,class}`.
  - The CPU is checked with CPUID.
  - The same `libs/hardware` table that `sysreport` uses classifies each
    function: **works**, **works with limits** (for example Wi-Fi WPA2
    only), or **not supported**, by class (storage, network, Wi-Fi,
    sound, display, input).
  - The report is shown before the ERASE step and can be saved as text.
  - **Blocking rules:**
    - no supported storage controller for the install disk: install refused;
    - no supported network: a warning, and the user must tick "I
      understand";
    - anything else: a warning only.
- **Security:** read-only enumeration. Nothing is sent anywhere; the user
  may share the saved file.
- **Tests:**
  - host tests of the classification against recorded device lists: the
    ZBook, QEMU q35, and an unsupported laptop;
  - a test that `compatibility.md` and the setup table cannot disagree, since
    both come from `libs/hardware`.
- **Accepted when:** on the ZBook the report matches what `sysreport`
  says after install.

### WP-4 — Updates from Settings, restarted when the user says (M–L)

- **Goal (P3, P6):** easy updates that never take the machine away.
- **Design:**
  - **Finding:** the update service fetches the release's `SHA256SUMS` and
    its signature over HTTPS (ADR-0028, 0031), as OceansSetup does
    (ADR-0128). It compares the version with `/bin/release`.
  - **When:** "Check automatically" is a Settings switch, on by default and
    at most once a day. This is the only other connection P1 allows. With
    it off, the system checks only when the user presses "Check now".
  - **Download** to the system disk, verified as ADR-0071 does. Nothing
    unverified is written to a slot.
  - **Apply** writes the other slot while the user keeps working. Then
    Settings offers **Restart now** and **Later**. "Later" means the next
    time the user restarts or switches off. The system never restarts
    by itself.
  - **After the restart,** a notice names the new version and says how to
    go back: Settings → Updates → "Use the previous version", which is
    `update previous` (ADR-0122).
- **Security:**
  - only update keys verify a system (ADR-0071);
  - no downgrade except to the kept previous slot, and only by the user;
  - applying needs `decide`, so an app or an AI agent cannot apply one.
- **Failure:** a failed download or verification leaves both slots as they
  were and says why. A power cut during apply is already safe (ADR-0071).
- **Tests:**
  - host tests for version comparison and the state machine
    (idle → found → downloaded → applied → restart pending);
  - smoke: serve a newer signed test release from the host, apply it from
    the Settings model, reboot, and see the new version, as
    `smoke-hw` already does for the shell path.
- **Accepted when:** an update installs from Settings with no shell, and
  the machine restarts only when told.

### WP-5 — A crashed driver comes back, and the user is told (M)

- **Goal (P6):** the CrowdStrike lesson, taken further than Windows'
  current plan. Oceans already keeps drivers out of the kernel.
- **Design:**
  - **Clients find a restarted provider.** For example, `fs` loses `nvme`.
    Today a restarted provider is given new endpoints (ADR-0016). Clients
    get a `PROVIDER_RESTARTED` event from init, ask init for the provider
    again, and retry calls that are safe to retry. Block reads are safe to
    retry. Writes are retried only when the block layer knows they did not
    complete.
  - **When a service gives up** (`max-restarts`): Core raises a
    notification ("Sound stopped working and was turned off; see
    Activity Monitor"). Activity Monitor lists the crashes from `diag
    crashes`.
- **Security:** re-finding goes through init's grants as at boot. A
  restarted driver gets no more than it had.
- **Tests:**
  - smoke: kill the storage driver during a file copy, and check the copy
    finishes or fails cleanly with no hang;
  - kill the sound driver past `max-restarts`, and check the notification
    appears.
- **Accepted when:** both smoke tests pass in all three smoke modes.

### WP-6 — Reproducible releases (M)

- **Goal (P9):** anyone can rebuild a release and get the same bytes.
- **Design:**
  - fix timestamps in archives, FAT images and GPT ids, using
    `SOURCE_DATE_EPOCH` from the commit time;
  - sort every directory walk;
  - pin the toolchain (already done in `rust-toolchain.toml`) and Go;
  - `cargo xtask release --verify` builds twice in different directories
    and compares SHA-256.
- **Tests:** CI runs the double build on every push to main.
- **Accepted when:** two CI runners produce identical `SHA256SUMS` for the
  same commit.

### WP-7 — Reduce transparency, increase contrast (S)

- **Goal (P10).**
- **Design:**
  - two switches in Settings → Appearance;
  - both are design values in `oceans-draw` (ADR-0113): opaque surfaces,
    and stronger text and borders. They are read by the desktop and the
    toolkit, so apps follow without changes.
- **Tests:** a host test that every text/background pair in every
  appearance (light, dark, × each switch) reaches WCAG 4.5:1. It fails the
  build when a value is changed below that.
- **Accepted when:** the test passes, and screenshots of all four
  appearances are in the ADR.

### WP-8 — Clean uninstall, reset app, defaults kept (S)

- **Goal (P3).**
- **Design:**
  - Settings → app → **Reset** deletes the app's data directory, after the
    user confirms.
  - **Remove** deletes the program and its data unless "keep my data" is
    ticked.
- **Tests:**
  - smoke: install, run (it writes data), remove, then check that no path
    with the app's id remains;
  - a default chosen with "always open with" (ADR-0099) survives an update
    of a system app.
- **Accepted when:** both tests pass.

### WP-9 — "What the user sees change" in every ADR (S)

- **Goal (P7).**
- **Design:**
  - `docs/adr/template.md` gains the section **What the user sees
    change**: moved, renamed or removed, and why;
  - release notes are gathered from these sections.
- **Accepted when:** the template is updated and the next UI ADR uses it.

### WP-10 — A support window for each hardware tier (S)

- **Goal (P8).**
- **Design:** `docs/hardware/support-tiers.md` states, for example, that a
  Tier 1 machine is supported for at least 5 years after it leaves the
  list, with security updates. It also says that the CPU baseline
  (x86-64-v2, UEFI) is raised only by an ADR, announced a year ahead.
- **Accepted when:** merged and linked from the README.

### WP-11 — A performance budget per release (M)

- **Goal:** the slowdowns users report after upgrading macOS and Windows
  are caught before an Oceans release ships.
- **Design:**
  - the smoke test records boot-to-desktop time, idle memory, and the
    desktop's frame time, in the serial log as the existing benchmarks are;
  - `xtask` compares the numbers with `docs/development/benchmarks.md`;
  - a release is held when a number is more than 15% worse, unless an ADR
    accepts it.
- **Tests:** CI runs the comparison, as a warning on pushes and a failure
  on release tags.
- **Accepted when:** the three numbers are in benchmarks.md, and the
  check runs.

## 4. Order

The roadmap approved on 2026-10-13 comes first. These WPs fit between its
items as follows:

| When | WPs | Why then |
|---|---|---|
| Now (documents only) | WP-9, WP-10, WP-1's ADR | Cheap, and they shape every later ADR |
| With update 0.1.2 | WP-1's network check | Before more network features arrive |
| After roadmap item 4 (battery) | WP-7, WP-8 | Small, visible improvements |
| After roadmap item 5 (Files) | WP-2 | Builds on Settings and Core |
| Before the browser (item 6) | WP-4 | The browser will need frequent updates |
| Alongside, as hardware grows | WP-3, WP-5 | Needed before others install Oceans |
| Before the first public release | WP-6, WP-11 | They make a release checkable |

## 5. Tracking

| WP | ADR | State |
|---|---|---|
| WP-1 Promises + idle-network check | [0146](../adr/0146-promises-to-the-user.md) | done |
| WP-2 Privacy & Permissions | [0156](../adr/0156-privacy.md) | done (exact use per app still open) |
| WP-3 Hardware check in Setup | — | planned |
| WP-4 Updates from Settings | [0158](../adr/0158-updates-from-settings.md) | done |
| WP-5 Driver comes back | — | planned |
| WP-6 Reproducible releases | — | planned |
| WP-7 Transparency and contrast | [0155](../adr/0155-legibility.md) | done |
| WP-8 Clean uninstall, reset | [0159](../adr/0159-clean-uninstall-and-reset.md) | done |
| WP-9 ADR "what the user sees change" | [0146](../adr/0146-promises-to-the-user.md) | done |
| WP-10 Support windows | [0146](../adr/0146-promises-to-the-user.md) | done |
| WP-11 Performance budget | — | planned |
