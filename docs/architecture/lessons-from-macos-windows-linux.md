# Lessons from macOS, Windows and Linux

What the three systems people already use get wrong, what Oceans already
does about it, and what is still to do. This is a positioning document, not
an ADR. Each item that becomes work gets an ADR of its own.

Researched October 2026. The sources are listed at the end. Complaints are
taken from reporting and commentary, not from measured surveys.

## The short version

| Their weakness | Who | Oceans today | Gap |
|---|---|---|---|
| Telemetry you cannot turn off, ads, upsell | Windows | No telemetry. Logs stay on the machine (ADR-0070, 0074) and leave only when the user saves them | Write it down as a promise |
| An online account forced at setup | Windows, partly macOS | Local accounts only (ADR-0110, 0115) | Make it a promise |
| AI that watches everything (Recall) | Windows | AI has no ambient authority. Every sensitive read is approved per call, with a log of each action (ADR-0007, 0051, 0055) | A promise: no screen capture or background indexing for AI |
| Forced updates and restarts | Windows | Signed updates in two slots, with the previous release kept (ADR-0071, 0092) | No scheduled-restart UI yet. The rule should be "never restart without the user" |
| A bad update or driver bricks the machine (CrowdStrike 2024) | Windows, Linux | Drivers run in userspace (ADR-0021). A third boot that does not finish starts safe mode by itself. The previous release is one menu entry away (ADR-0122) | Restart already exists (ADR-0016). Missing: clients that find the restarted driver again, and telling the user |
| Third-party code in the kernel | Windows (anti-virus), Linux (out-of-tree modules) | Microkernel-leaning design. Device access is a capability (ADR-0002, 0011, 0021) | None. Keep it that way |
| Permission prompt fatigue (monthly screen-recording prompts) | macOS | Asked once at first run. A decision stands until the user changes it (ADR-0047) | Settings already changes each app's permissions (ADR-0081). Missing: a view by permission, with when each was last used |
| Apps that read the clipboard whenever they like | All three | Paste is pushed only into the focused app, after the user pastes (ADR-0095) | None. This is a strength to show |
| Fragmentation: many desktops, toolkits and package formats | Linux | One desktop, one toolkit (`oceans-ui`), one package format (`.opk`), one API level (ADR-0045, 0046, 0105) | Keep it to one of each |
| Drivers missing for Wi-Fi, sleep, printers and GPUs | Linux | Same problem, and worse: a deliberately small Tier 1 list (ADR-0005, 0068) | This is our biggest weakness (below) |
| Hardware dropped by policy (TPM 2.0 cut-off; Intel Macs end after Tahoe) | Windows, macOS | Requires x86-64-v2 + UEFI only. Runs on a 2016 ZBook | State a support window for each tier |
| Lock-in to one vendor's hardware | macOS | Runs on ordinary PCs. Installs over Windows or Linux (ADR-0128, 0131) | — |
| Look over legibility (Liquid Glass) | macOS | Big Sur language, light translucency (ADR-0078, 0105) | A "reduce transparency" switch and contrast checks in the a11y work |
| UI churn: features moved or removed between releases | Windows 11, macOS Tahoe | Every change is recorded in an ADR | A rule: nothing the user relies on moves without a reason in an ADR |
| Settings split across two places | Windows (Control Panel vs Settings) | One Settings app (ADR-0081) | Keep every setting in exactly one place |
| The system overrides the user's default apps (Edge) | Windows | Kinds declared in the manifest. The user chooses which app opens them (ADR-0099) | System apps never take a default back |
| Supply-chain attacks through dependencies (xz 2024) | Linux | Minimal, audited dependencies (ADR-0004). Packages and releases are signed (ADR-0046, 0072, 0075) | Reproducible builds, so anyone can check a release |
| Rot: leftovers after uninstall, "reinstall every year" | Windows | Each app has its own directory and data (ADR-0045, 0047) | A guaranteed clean uninstall, and "reset app" |
| Slowdown and battery drain after upgrading | macOS Tahoe, Windows | Small native system with no web engine on the device (ADR-0105) | Benchmarks per release (master spec §51), so a regression is caught before it ships |

## What Oceans is, that none of them are

1. **Capabilities from the kernel up.** A process can only do what it holds
   a handle for (ADR-0006, 0011). macOS and Windows add permissions on top
   of ambient authority. Oceans starts with none.
2. **AI as an unprivileged, mediated principal.** The system writes the
   approval text, not the agent. An agent can never answer consent. Every
   call is logged (ADR-0007, 0051). Copilot, Apple Intelligence and Recall
   were added to systems not designed for this.
3. **Updates you can always undo.** Two slots, signed, never a downgrade,
   and safe mode after three failed boots (ADR-0071, 0092, 0122).
4. **Memory-safe Rust** in the kernel, drivers and system services.
5. **Thai as a first-class language**: interface text and Kedmanee typing
   (ADR-0077, 0114), not an afterthought locale.
6. **The device stays in control of the web.** The full system is available
   from a paired browser, but the device confirms installs (ADR-0058,
   0061).

## Where we are weaker, and what we do about it

| Weakness | What to do |
|---|---|
| Little hardware works: Wi-Fi is one Intel chip (ADR-0129), there is no sleep yet, GPU drawing is software only | Linux's lesson: the failure is buying the wrong machine. **Check before install.** OceansSetup already checks the disk and firmware (ADR-0128). Extend it to list each device as works or does not work, and publish the Tier 1 list as a hardware guide |
| No web browser | Roadmap item 6: a read-only browser (HTML and CSS) |
| Few apps | The SDK and signed developer keys exist (ADR-0062, 0063). Write the first useful apps ourselves |
| Filesystem limits (~2 GiB of data, 256 MiB per file) | Roadmap item 3: extents |
| No battery or brightness controls | Roadmap item 4 |

## Proposed next work

The full plan, with designs, tests and acceptance criteria for each item:
[development/plan-lessons-from-other-systems.md](../development/plan-lessons-from-other-systems.md).

In order, after the current roadmap items (MP3/radio → Wi-Fi → extents →
battery → Files → browser):

1. **ADR: Oceans' promises to the user.** No ads. No telemetry. No forced
   online account. No forced restart. AI never watches the screen and never
   reads without approval. Default apps belong to the user. Each promise
   has a test or a check that guards it where it can.
2. **Privacy & Permissions in Settings.** Every grant of every app, when it
   was last used, and revoke with one click. The AI activity log sits next
   to it.
3. **A hardware check in OceansSetup**, before it erases anything: a
   per-device report of what works, can be saved, and can be sent by the
   user.
4. **Update timing.** Download in the background, then "restart now" or
   "tonight". The system never restarts on its own while windows are open.
5. **Driver restart policy.** A driver that crashes is started again. After
   repeated crashes it is left off with a notification, never a hung
   machine.
6. **Reproducible release builds**, checked in CI.
7. **Accessibility: reduce transparency and increase contrast**, in the
   design values (ADR-0113).

## Sources

- Thurrott, *Making Windows 11 Suck Less in 2026* — https://www.thurrott.com/windows/windows-11/332300/making-windows-11-suck-less-in-2026
- TWiT, *What Microsoft isn't fixing in Windows 11* — https://twit.tv/posts/tech/what-microsoft-isnt-fixing-windows-11-unspoken-problems-explained
- VAMK, *The state of Windows in 2026* — https://energiaa.vamk.fi/en/blogs/the-state-of-windows-in-2026/
- Cybersecurity Dive, Windows Resiliency Initiative — https://www.cybersecuritydive.com/news/microsoft-windows-resilience-initiative-security-kernel/813416/
- Help Net Security, limiting security products' kernel access — https://www.helpnetsecurity.com/2024/11/19/windows-kernel-security-vendors/
- CleanMyMac, *macOS Tahoe issues* — https://cleanmymac.com/blog/macos-tahoe-issues
- 9to5Mac, Sequoia's monthly screen-recording prompt — https://9to5mac.com/2024/08/14/macos-sequoia-screen-recording-prompt-monthly/
- Six Colors, *macOS Tahoe is the last release to support Intel* — https://sixcolors.com/post/2025/06/macos-tahoe-is-the-last-release-to-support-intel/
- Webiano, *The Linux desktop still loses the mass market at the hardware layer* (April 2026) — https://webiano.digital/the-linux-desktop-still-loses-the-mass-market-at-the-hardware-layer/
- Windows News, *Seven barriers for the Linux desktop in 2026* — https://windowsnews.ai/article/linux-desktop-faces-seven-barriers-in-2026-as-windows-maintains-dominant-position.395648
