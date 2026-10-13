# Architecture Decision Records

Every decision that is hard to reverse or shapes the system's identity gets an
ADR before implementation (master spec §48, §53). AI coding agents must follow
accepted ADRs and may not change them without a new ADR.

Statuses: **Proposed** → **Accepted** → (**Superseded by ADR-NNNN**).

| ADR | Title | Status |
|---|---|---|
| [0001](0001-monorepo-and-language-boundaries.md) | Monorepo and language boundaries | Accepted |
| [0002](0002-kernel-architecture.md) | Kernel architecture: microkernel-leaning hybrid | Accepted |
| [0003](0003-boot-protocol.md) | Boot protocol: Limine behind a boot abstraction | Accepted |
| [0004](0004-rust-toolchain.md) | Stable Rust only; minimal, audited dependencies | Accepted |
| [0005](0005-hardware-targets.md) | Hardware tiers and initial targets | Accepted |
| [0006](0006-security-model.md) | Capability-based security model | Accepted (mechanism: ADR-0011) |
| [0007](0007-ai-permission-mediation.md) | AI agents as unprivileged, mediated principals | Accepted |
| [0008](0008-physical-frame-allocator.md) | Physical frame allocator: buddy system with per-frame metadata | Accepted |
| [0009](0009-kernel-address-space.md) | Kernel address space: own page tables, W^X, guarded stacks | Accepted |
| [0010](0010-kernel-heap.md) | Kernel heap: slab caches + buddy page blocks via the direct map | Accepted |
| [0011](0011-capabilities.md) | Kernel objects and capabilities | Accepted |
| [0012](0012-threads-and-scheduling.md) | Kernel threads and preemptive round-robin scheduling | Accepted |
| [0013](0013-ipc.md) | IPC: endpoints (call/reply) and notifications | Accepted |
| [0014](0014-processes-and-user-mode.md) | Processes, user mode and the system call ABI | Accepted |
| [0015](0015-syscall-abi-v2.md) | System call ABI v2: capabilities over IPC, memory, processes | Accepted |
| [0016](0016-init-and-service-manager.md) | init and the service manager | Accepted |
| [0017](0017-console-input.md) | Console input: ACPI, I/O APIC and a console capability | Accepted |
| [0018](0018-shell.md) | The shell: a capability-explicit command line | Accepted |
| [0019](0019-filesystem.md) | Filesystem service: node handles as capabilities | Accepted |
| [0020](0020-system-information-and-utilities.md) | System information, program manifests and basic utilities | Accepted |
| [0021](0021-pci-and-userspace-drivers.md) | PCI, device capabilities and userspace drivers | Accepted (amended by 0022) |
| [0022](0022-filesystem-on-disk.md) | The filesystem on disk (OceansFS) | Accepted |
| [0023](0023-network.md) | Networking: virtio-net, the IPv4 stack and sockets | Accepted |
| [0024](0024-tcp-and-dns.md) | TCP and DNS | Accepted |
| [0025](0025-boot-archive.md) | The boot archive (initrd) | Accepted |
| [0026](0026-entropy.md) | Kernel entropy and the `RANDOM` system call | Accepted |
| [0027](0027-oceansfs-data-checksums.md) | OceansFS format 2: data block checksums | Accepted |
| [0028](0028-http-client.md) | HTTP client | Accepted |
| [0029](0029-framebuffer-console.md) | Framebuffer console and PS/2 keyboard | Accepted |
| [0030](0030-shared-buffers.md) | Shared buffers for bulk data | Accepted |
| [0031](0031-tls.md) | TLS and wall-clock time | Accepted |
| [0032](0032-usb.md) | USB: an xHCI driver and boot keyboards | Accepted |
| [0033](0033-usb-hubs.md) | USB hubs | Accepted |
| [0034](0034-usb-mass-storage.md) | USB mass storage and class drivers | Accepted |
| [0035](0035-mounting-removable-media.md) | Mounting removable media | Accepted |
| [0036](0036-read-only-fat.md) | Read-only FAT | Accepted |
| [0037](0037-crash-safe-fat-writes.md) | Crash-safe FAT writes | Accepted |
| [0038](0038-rename.md) | Rename | Accepted |
| [0039](0039-copying-with-shared-buffers.md) | Copying with shared buffers | Accepted |
| [0040](0040-nvme.md) | NVMe | Accepted |
| [0041](0041-intel-ethernet.md) | Intel Ethernet (82574L / e1000e) | Accepted |
| [0042](0042-usb-mice-and-pointer-input.md) | USB mice and pointer input | Accepted |
| [0043](0043-ipv6.md) | IPv6 | Accepted |
| [0044](0044-stopping-processes.md) | Stopping processes (ABI 12) | Accepted |
| [0045](0045-oceans-runtime.md) | The Oceans Runtime: apps, Oceans Core and API level 1 | Accepted |
| [0046](0046-packages.md) | Packages (.opk) and publisher signatures | Accepted |
| [0047](0047-permissions-and-consent.md) | Permissions and consent | Accepted |
| [0048](0048-delegating-core-authority.md) | Delegating Oceans Core authority | Accepted |
| [0049](0049-app-services.md) | App services | Accepted |
| [0050](0050-go-on-oceans.md) | Go on Oceans (WebAssembly in a Rust host) | Accepted |
| [0051](0051-ai-runtime.md) | The AI runtime: agents, tools, approvals and the model gateway | Accepted |
| [0052](0052-go-apps-as-packages.md) | Go apps as packages | Accepted |
| [0053](0053-directory-grants.md) | Directory grants from init, and the AI's settings | Accepted |
| [0054](0054-ai-gateway-dns-and-https.md) | The AI model gateway: DNS and https | Accepted |
| [0055](0055-sensitive-reads.md) | Sensitive reads by AI agents | Accepted |
| [0056](0056-ui-architecture.md) | UI architecture: a native desktop, and SvelteKit apps through a bridge | Accepted |
| [0057](0057-display-and-desktop.md) | The display service and the desktop (ABI 13) | Accepted |
| [0058](0058-web-experience-and-bridge.md) | The Oceans web experience: SvelteKit apps and the Go bridge | Accepted (amended by 0061) |
| [0059](0059-keyboard-focus-and-app-windows.md) | Keyboard focus and app windows (ABI 14) | Accepted |
| [0060](0060-go-windows.md) | Windows for Go apps | Accepted |
| [0061](0061-store.md) | The Store, and the end of Phase 7 | Accepted |
| [0062](0062-sdk-and-developer-tools.md) | The Oceans SDK and developer tools | Accepted (amended by 0064) |
| [0063](0063-developer-keys.md) | Trusting developers' keys | Accepted |
| [0064](0064-web-apps.md) | Web apps (SvelteKit and Bun), and the end of Phase 8 | Accepted |
| [0065](0065-notifications.md) | Notifications for apps | Accepted |
| [0066](0066-web-apps-network.md) | The network for web apps | Accepted |
| [0067](0067-expiring-developer-keys.md) | Developer keys that expire | Accepted |
| [0068](0068-hardware-validation.md) | Hardware validation and the Tier 1 list | Accepted |
| [0069](0069-ahci.md) | AHCI (SATA) | Accepted |
| [0070](0070-diagnostics.md) | Diagnostics: the kept log and `diag` (ABI 15) | Accepted |
| [0071](0071-system-updates.md) | System updates: signed, two slots, the previous release kept | Accepted |
| [0072](0072-release-keys.md) | Release keys and release images | Accepted |
| [0073](0073-alpha-hardening.md) | Alpha hardening: small BARs, reserved system ids | Accepted |
| [0074](0074-kept-logs-on-disk.md) | The log kept on disk across reboots | Accepted |
| [0075](0075-signed-release-checksums.md) | Signed release checksums and the published release key | Accepted |
| [0076](0076-desktop-shell.md) | The desktop shell: taskbar, Start menu, windows that minimize | Accepted (amended by 0078) |
| [0077](0077-interface-text.md) | Interface text: Noto Sans, and Thai | Accepted |
| [0078](0078-desktop-look.md) | The desktop's look: a menu bar, a dock, light windows | Accepted |
| [0079](0079-audio.md) | Sound: Intel High Definition Audio | Accepted |
| [0080](0080-app-toolkit-and-system-apps.md) | The app toolkit, and apps that come with the system | Accepted |
| [0081](0081-settings-and-system-rights.md) | Settings, and the rights of the system's own apps | Accepted |
| [0082](0082-files.md) | Files | Accepted |
| [0083](0083-activity-monitor.md) | Activity Monitor | Accepted |
| [0084](0084-text-editor.md) | Text Editor, and the keys that move | Accepted |
| [0085](0085-switching-off-and-restarting.md) | Switching off and restarting (ABI 16) | Accepted |
| [0086](0086-stopping-services-gracefully.md) | Stopping services gracefully | Accepted |
| [0087](0087-audio-input.md) | Sound input: recording from HD Audio | Accepted |
| [0088](0088-every-cpu-up.md) | Every CPU up | Accepted |
| [0089](0089-scheduling-on-every-cpu.md) | Scheduling on every CPU | Accepted |
| [0090](0090-image-viewer.md) | Image Viewer | Accepted |
| [0091](0091-secure-boot.md) | Secure Boot | Accepted |
| [0092](0092-secure-boot-updates.md) | Updates under Secure Boot: a slot per partition, switched in the partition table | Accepted |
| [0093](0093-serial-input-without-loss.md) | Serial input without loss: flow-controlled test consoles, a lower FIFO trigger, overruns reported | Accepted |
| [0094](0094-music-and-sound-for-apps.md) | Music, and sound for apps: a player end that plays and cannot record | Accepted |
| [0095](0095-clipboard.md) | The clipboard: copied while focused, pasted only where the user pastes | Accepted |
| [0096](0096-settings-network-and-sound.md) | Settings: the network and the sound, through reader ends | Accepted |
| [0097](0097-resizing-windows.md) | Resizing and maximizing windows: edges, the zoom button, a double click; apps that ask | Accepted |
| [0099](0099-opening-files-in-apps.md) | Opening files in apps: kinds declared in the manifest, opened through the desktop after the user's click | Accepted |
| [0100](0100-system-volume.md) | The system volume: applied by the driver, kept by Core, set from the menu bar | Accepted |
| [0101](0101-volume-keys.md) | The volume keys: PS/2 and USB, the desktop's whoever has the focus | Accepted |
| [0102](0102-media-keys.md) | Media keys: HID consumer controls, and keys for the player | Accepted |
| [0103](0103-now-playing.md) | What plays, in the sound panel: the player's title and buttons | Accepted |
| [0104](0104-kills-are-never-lost.md) | A kill is never lost: the scheduler looks again under its lock | Accepted |
| [0105](0105-ui-direction.md) | The direction of the UI: native on the device, the web from afar; Big Sur polished; accessibility now | Accepted |
| [0106](0106-keyboard-focus-and-roles.md) | The keyboard reaches every widget: Tab and Shift+Tab, roles and names, a focus ring | Accepted |
| [0107](0107-arranging-windows.md) | Arranging windows: halves and quarters by dragging, Super shortcuts, full screen; windows up to 4K | Accepted |
| [0108](0108-search-and-launcher.md) | Search in the launcher: Super alone, apps and the files of Home (Core's `FIND`), Enter opens | Accepted |
| [0109](0109-apps-kept-in-the-dock.md) | Apps kept in the dock: a right click keeps or takes out, Core keeps them across reboots | Accepted |
| [0110](0110-lock-screen-and-password.md) | The lock screen and the user's password: Argon2id kept by Core, Super+L, waits after wrong passwords | Accepted |
| [0111](0111-the-account-in-settings.md) | The account in Settings: changing the password needs the current one (`CHANGE_PASSWORD`), a password field | Accepted |
| [0112](0112-locking-after-a-while.md) | Locking after a while without input, chosen in Settings; Lock in the apps panel | Accepted |
| [0113](0113-dark-appearance-and-accent.md) | The dark appearance and the accent: one set of design values, kept by Core, every window told (`kind::THEME`) | Accepted |
| [0114](0114-typing-thai.md) | Typing Thai: Kedmanee beside English, Super+Space or the menu bar to switch, characters to the focused window (`kind::TEXT`) | Accepted |
| [0115](0115-several-users.md) | Several users: accounts kept by Core, each with its own Home; the lock screen signs one in, Settings and the shell administer | Accepted |
| [0116](0116-each-users-own-settings.md) | Each user's own settings: the dock, the appearance and the keyboard layout kept per account; the layout kept across restarts | Accepted |
| [0117](0117-wave-and-launchpad.md) | Wave, Oceans' own taskbar (one button that rises into a crest of icons), and Launchpad (the apps across the screen, with search) | Accepted |
| [0118](0118-control-center.md) | Control Center and the status icons: round toggles, what plays, keyboard, accent and sound; the network read through a reader end (`NET_READER`) | Accepted |
| [0119](0119-notifications-at-the-bell.md) | The notifications at the bell: kept, newest first, a count of new ones, a phone-like panel with Clear | Accepted |
| [0120](0120-icons-on-the-desktop.md) | Icons on the desktop: Home and the apps kept in Wave, down its right side; a double click opens | Accepted |
| [0121](0121-top-bar-as-macos.md) | The top bar as macOS has it: the time and the app at the left; the layout, speaker, user, network, Control Center and bell at the right; the user's menu | Accepted |
| [0122](0122-recovery.md) | Recovery (ABI 17): safe mode, by choice or after three unfinished boots; recovery mode (the disks, the network, a shell); `recovery` for a forgotten password and a factory reset; `update previous` | Accepted |
| [0123](0123-ethernet-for-real-pcs.md) | Ethernet for real PCs: `oceans-nic`; the I217/I218/I219 in e1000e; igc (82576, I210/I211, I225/I226); rtl8168 (RTL8111/8168); `grant = device:A,B,...` | Accepted |
| [0124](0124-msi.md) | MSI for devices without MSI-X (ABI 18): `DEVICE_IRQ` binds the one message and says which it bound; AHCI, the I219 and 8168s on interrupts | Accepted |
| [0125](0125-pci-config-writes.md) | Writing PCI configuration space, the safe fields only (ABI 19): `DEVICE_CONFIG_WRITE` for the PCI Express power and request-size fields; rtl8168 turns ASPM off | Accepted |
| [0126](0126-installed-on-the-internal-disk.md) | Oceans installed on the internal disk (ABI 20): GPT with an ESP and an Oceans root partition; the boot partition from the kernel; `/boot`; updates there | Accepted |
| [0127](0127-the-installer.md) | The installer (ABI 21): install mode; the release checked against the image's keys; the SSD Oceans started from laid out as installed | Accepted |
| [0128](0128-oceans-setup.md) | Oceans Setup: installing from the web over Windows (`OceansSetup.exe`: checks, verified download, ERASE, a staging partition, UEFI boot entries, restart) | Accepted |
| [0129](0129-wi-fi.md) | Wi-Fi: the Intel Wireless 8260 (`iwm`, Intel's firmware as a module), WPA2-Personal in software (`oceans-wpa`), iwm between the stack and the wired NIC; the network in `/system/wifi` | Accepted |
| [0130](0130-wi-fi-panel.md) | Choosing a Wi-Fi network in Control Center: files in `/system/wifi` between the driver and the desktop; the wallpaper a module of the boot image | Accepted |
| [0131](0131-oceans-setup-on-linux.md) | Oceans Setup on Linux (`OceansSetup-linux`, in a terminal: the installer staged in Linux's own EFI system partition, boot entries through efivarfs; `cargo xtask setup-linux` to try it) | Accepted |
| [0132](0132-ps2-touchpad.md) | The PS/2 touchpad and mouse (ABI 22): the i8042's auxiliary port in the kernel (the wheel knock, IRQ 12 on the keyboard's vector), `oceans_input::ps2`, `DISPLAY_POINTER` to the desktop; the keyboard no longer blocked by touchpad bytes | Accepted |
| [0133](0133-shell-line-editing.md) | The shell's history and Tab completion (`oceans-line`: Up/Down recall lines, Tab completes commands, programs and paths) | Accepted |
| [0134](0134-larger-files.md) | Files up to 256 MiB (OceansFS's per-file limit raised from 16 MiB; the format unchanged) | Accepted |
| [0135](0135-mp3-and-radio.md) | MP3 and the radio in Music (`oceans-mp3` on nanomp3-core, `oceans-web` for apps, `ICY` heads; stations kept in `Music/Stations.txt`) | Accepted |

New ADRs copy [template.md](template.md) and take the next number.
