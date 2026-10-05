//! `cargo xtask` — Oceans build and emulator tooling.
//!
//! Cross-platform (Windows, Linux, macOS) and dependency-free so it works on
//! any machine with a Rust toolchain. See docs/development/getting-started.md.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

mod hardware;
mod release;

const KERNEL_TARGET: &str = "x86_64-unknown-none";
const KERNEL_PACKAGE: &str = "oceans-kernel";
/// User programs (in the `user/` workspace) shipped as boot modules.
const USER_PROGRAMS: &[&str] = &[
    "init",
    "echo-service",
    "hello-client",
    "crasher",
    "fs",
    "shell",
    "ps",
    "mem",
    "uptime",
    "uname",
    "lspci",
    "disk",
    "virtio-blk",
    "nvme",
    "ahci",
    "virtio-net",
    "e1000e",
    "xhci",
    "usb-storage",
    "core",
    "display",
    "gohost",
    "usb-hid",
    "net",
    "net-echo",
    "ifconfig",
    "ping",
    "host",
    "nc",
    "fetch",
    "date",
    "lsusb",
    "sysreport",
    "diag",
    "update",
    "apps",
    "mouse",
    "ipc-test",
];
/// The virtio disk QEMU attaches (ADR-0021), holding the filesystem
/// (ADR-0022). `run` keeps its disk across boots: it is the system's
/// storage. `smoke` starts from a fresh blank disk, boots twice, and mounts
/// the result on the host.
const DISK_IMAGE: &str = "build/disk.img";
const SMOKE_DISK_IMAGE: &str = "build/smoke-disk.img";
/// Room for Go apps too (ADR-0052): their packages are a few MB each.
const DISK_SIZE: u64 = 32 * 1024 * 1024;
/// The USB stick QEMU plugs into the xHCI controller (ADR-0034): `run`
/// keeps its own, `smoke` starts from a blank one: the first boot gets it
/// formatted through `/usb` (ADR-0035) and stores `STICK_TEXT` and a
/// download there, the second reads them back, and the host checks them.
const STICK_IMAGE: &str = "build/usb-stick.img";
const SMOKE_STICK_IMAGE: &str = "build/smoke-stick.img";
const STICK_SIZE: usize = 4 * 1024 * 1024;
const STICK_TEXT: &str = "kept on a usb stick";
/// The NVMe disk (ADR-0040), holding a second Oceans volume at /nvme: `run`
/// keeps its own, `smoke` starts from a blank one, which the first boot
/// formats and writes, the second reads back, and the host checks.
const NVME_IMAGE: &str = "build/nvme.img";
const SMOKE_NVME_IMAGE: &str = "build/smoke-nvme.img";
const NVME_SIZE: usize = 16 * 1024 * 1024;
const NVME_TEXT: &str = "kept on nvme";
/// The development package signing key (ADR-0046, tools/keys/README.md):
/// public, for examples and tests only. Images trust it as `DEV_PUBLISHER`.
const DEV_SEED: &str = include_str!("../../keys/oceans-dev.seed");
const DEV_PUBLISHER: &str = "Oceans Examples";
/// The release an image is (ADR-0071): `/bin/release`.
const RELEASE_VERSION: &str = env!("CARGO_PKG_VERSION");
const RELEASE_CHANNEL: &str = "alpha";
/// A key no image trusts (the smoke test's refused package).
const UNTRUSTED_SEED: [u8; 32] = [0x55; 32];
/// Go programs (ADR-0050), built for wasip1 and run by `gohost`: the
/// package under `go/` and the module file name in the boot archive.
const GO_PROGRAMS: &[(&str, &str)] = &[
    ("./cmd/gohello", "gohello.wasm"),
    ("./cmd/ai", "ai.wasm"),
    ("./cmd/bridge", "bridge.wasm"),
];
/// The web experience (ADR-0058): a SvelteKit app in `ui/`, built by Bun
/// into static files that the bridge embeds from `BRIDGE_WEB` (so the UI
/// is built before the Go programs).
const UI_DIR: &str = "ui";
const BRIDGE_WEB: &str = "go/cmd/bridge/web";
/// The port the bridge listens on in the guest, and the host port `run`
/// forwards to it (`OCEANS_BRIDGE_PORT`, default the same).
const BRIDGE_PORT: u16 = 8080;
/// The bridge's port for web apps (ADR-0064), forwarded beside it.
const BRIDGE_APP_PORT: u16 = 8081;
/// Go apps (ADR-0052): built the same way into `build/go`, but shipped as
/// packages, not in the boot archive.
const GO_APPS: &[(&str, &str)] = &[
    ("./apps/greeter", "greeter.wasm"),
    ("./apps/tiles", "tiles.wasm"),
];
/// Signed example packages, built with every image (and served to the
/// smoke test's guest over HTTP).
const PACKAGES_DIR: &str = "build/packages";
/// The example app (user/apps/hello) and its manifest.
const HELLO_PROGRAM: &str = "hello-app";
const HELLO_MANIFEST: &str = include_str!("../../../user/apps/hello/manifest");
/// The example service (user/apps/heartbeat, ADR-0049).
const HEARTBEAT_PROGRAM: &str = "heartbeat-app";
const HEARTBEAT_MANIFEST: &str = include_str!("../../../user/apps/heartbeat/manifest");
/// The example windowed app (ADR-0059).
const NOTES_PROGRAM: &str = "notes-app";
const NOTES_MANIFEST: &str = include_str!("../../../user/apps/notes/manifest");
/// The example Go app (go/apps/greeter, ADR-0052): a `wasm` program.
const GREETER_PROGRAM: &str = "greeter.wasm";
/// The example windowed Go app (ADR-0060).
const TILES_PROGRAM: &str = "tiles.wasm";
const TILES_MANIFEST: &str = include_str!("../../../go/apps/tiles/manifest");
const GREETER_MANIFEST: &str = include_str!("../../../go/apps/greeter/manifest");
/// A second stick, plugged in during the first smoke boot: FAT16 in an MBR
/// partition, made by mkfs.fat and mtools (libs/fat/testdata, ADR-0036).
const SMOKE_FAT_IMAGE: &str = "build/smoke-fat.img";
const FAT_FIXTURE: &[u8] = include_bytes!("../../../libs/fat/testdata/fat16.sparse");
/// What the first smoke boot stores and the second reads back.
const KEPT_PATH: [&str; 2] = ["keep", "note.txt"];
const KEPT_TEXT: &str = "kept across reboots";
/// Service manifests for init: normal boots and smoke tests.
const MANIFEST: &str = "config/services.conf";
const SMOKE_MANIFEST: &str = "config/services-smoke.conf";
const LIMINE_REPO: &str = "https://github.com/limine-bootloader/limine.git";
const LIMINE_BRANCH: &str = "v9.x-binary";
const ONLINE_BANNER: &str = "OCEANS KERNEL ONLINE";
/// The smoke test types `SHELL_SCRIPT` into the serial console when the
/// kernel log shows `SHELL_READY` (user/shell, ADR-0018), then requires
/// every `SHELL_EXPECT` line in the console output.
const SHELL_READY: &str = "shell: ready";
/// The last boot-time log lines: the USB device behind the hub, the
/// stick's and the tablet's class drivers, and the end of the crasher's
/// restarts. They would otherwise land in the middle of the first
/// commands' output, and while the log is busy QEMU (on Windows) drops
/// typed bytes, so typing waits.
const USB_SETTLED: [&str; 4] = [
    "xhci: port 6.1: ",
    "usb-storage: port 3: ",
    "usb-hid: port 6.1: ",
    "crasher exited with -142; giving up",
];
/// Delay between typed bytes.
const TYPING_DELAY: Duration = Duration::from_millis(2);
/// Lines end in CR LF: Enter is CR on a serial terminal, and QEMU's Windows
/// stdio backend drops a lone CR from piped input. 0x7f is Backspace.
const SHELL_SCRIPT: &[&[u8]] = &[
    b"help\r\n",
    b"echo hello from the shell\r\n",
    b"echo abc\x7fd\r\n",
    b"grants\r\n",
    b"call echo ping\r\n",
    // Filesystem (ADR-0019).
    b"ls /bin\r\n",
    b"mkdir /docs\r\n",
    b"write /docs/note.txt hello filesystem\r\n",
    b"cat /docs/note.txt\r\n",
    b"ls /docs\r\n",
    b"rm /docs/note.txt\r\n",
    b"cat /docs/note.txt\r\n",
    b"write /bin/evil x\r\n",
    b"rm /bin/crasher\r\n",
    // Diagnostics (ADR-0070): the log the kernel keeps, read only with the
    // `logs` grant; the crasher test service's faults are in it.
    b"run diag out -- crashes\r\n",
    b"run diag out logs -- crashes\r\n",
    b"run diag out logs sysinfo use:fs -- save /docs/diag.txt\r\n",
    // Programs come from /bin and get only the typed authority.
    b"run hello-client log use:echo\r\n",
    b"run /bin/crasher log\r\n",
    b"run hello-client use:nothing\r\n",
    // Utilities (ADR-0020): bare commands get only what their manifest
    // requests, and only low-risk grants.
    b"uname\r\n",
    b"uptime\r\n",
    b"mem\r\n",
    b"ps\r\n",
    b"run ps out\r\n",
    b"hello-client\r\n",
    b"run nosuch\r\n",
    b"frobnicate\r\n",
    // Devices (ADR-0021). Raw disk access is the filesystem's alone.
    b"lspci\r\n",
    b"run lspci out devices\r\n",
    b"run disk out\r\n",
    b"run disk out use:block -- info\r\n",
    // Files on disk (ADR-0022), read back by the second boot.
    b"mkdir /keep\r\n",
    b"write /keep/note.txt kept across reboots\r\n",
    b"write /keep/gone.txt temporary\r\n",
    b"rm /keep/gone.txt\r\n",
    b"sync\r\n",
    // The network (ADR-0023): QEMU's user network answers DHCP and pings
    // to its gateway; 10.0.2.99 does not exist.
    b"ifconfig\r\n",
    b"run ifconfig out use:net\r\n",
    b"run ping out use:net -- 10.0.2.2 2\r\n",
    b"run ping out use:net -- 10.0.2.99 1\r\n",
    // TCP and DNS (ADR-0024), against servers xtask runs on the host
    // (reached as 10.0.2.2; `$DNS` and `$TCP` are their ports).
    b"run host out use:net -- oceans.test 10.0.2.2:$DNS\r\n",
    b"run host out use:net -- missing.test 10.0.2.2:$DNS\r\n",
    b"run nc out use:net -- 10.0.2.2 $TCP hello over tcp\r\n",
    // HTTP (ADR-0028), from a server xtask runs on the host.
    b"run fetch out use:net -- http://10.0.2.2:$HTTP/hello.txt\r\n",
    b"run fetch out use:net -- http://10.0.2.2:$HTTP/redirect\r\n",
    b"run fetch out use:net -- http://10.0.2.2:$HTTP/chunked\r\n",
    b"run fetch out use:net -- http://10.0.2.2:$HTTP/missing\r\n",
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/big /keep/big.bin\r\n",
    // TLS (ADR-0031) against OpenSSL's `s_server` on the host: its test CA
    // is not one of the built-in roots until `--ca` adds it.
    b"date\r\n",
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/ca.pem /keep/ca.pem\r\n",
    b"run fetch out use:net -- https://10.0.2.2:$HTTPS/tls.txt\r\n",
    b"run fetch out use:net use:fs -- --ca /keep/ca.pem https://10.0.2.2:$HTTPS/tls.txt\r\n",
    b"run fetch out use:net use:fs -- --ca /keep/ca.pem https://10.0.2.2:$HTTPS/tls.bin /keep/tls.bin\r\n",
    // NVMe (ADR-0040): the driver serves the block protocol, and a second
    // fs instance keeps an Oceans volume on it at /nvme.
    b"run disk out use:nvme -- info\r\n",
    b"write /nvme/note.txt kept on nvme\r\n",
    b"cp /keep/big.bin /nvme\r\n",
    b"ls /nvme\r\n",
    // IPv6 (ADR-0043): QEMU's user network advertises fec0::/64 from
    // router fe80::2 and maps fec0::2 to the host's ::1, where xtask runs
    // IPv6 servers (`$DNS6`, `$TCP6`, `$HTTP6`).
    b"run ifconfig out use:net\r\n",
    b"run ping out use:net -- fec0::2 2\r\n",
    b"run ping out use:net -- fe80::2 1\r\n",
    b"run host out use:net -- ipv6.oceans.test [fec0::2]:$DNS6\r\n",
    b"run nc out use:net -- [fec0::2] $TCP6 hello over ipv6\r\n",
    b"run fetch out use:net -- http://[fec0::2]:$HTTP6/ipv6.txt\r\n",
    // Apps (ADR-0045 to ADR-0047): signed packages from the host; refused
    // ones; a permission asked for (denied), granted, revoked while the
    // app runs; an update, a refused downgrade, a rollback; the audit.
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/packages/hello-1.0.0.opk /keep/hello.opk\r\n",
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/packages/hello-2.0.0.opk /keep/hello2.opk\r\n",
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/packages/untrusted.opk /keep/untrusted.opk\r\n",
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/packages/tampered.opk /keep/tampered.opk\r\n",
    b"app install /keep/untrusted.opk\r\n",
    b"app install /keep/tampered.opk\r\n",
    b"app install /keep/hello.opk\r\n",
    b"app list\r\n",
    b"app run app.oceans.hello 10.0.2.2 $TCP\r\n",
    b"n\r\n",
    b"app grant app.oceans.hello network\r\n",
    b"app run app.oceans.hello 10.0.2.2 $TCP\r\n",
    b"app info app.oceans.hello\r\n",
    b"app install /keep/hello2.opk\r\n",
    b"app install /keep/hello.opk\r\n",
    b"app run app.oceans.hello\r\n",
    b"app start app.oceans.hello wait\r\n",
    b"app list\r\n",
    b"app revoke app.oceans.hello network\r\n",
    b"app rollback app.oceans.hello\r\n",
    b"app start app.oceans.hello wait\r\n",
    b"app stop app.oceans.hello\r\n",
    b"app audit\r\n",
    // Narrower Core capabilities (ADR-0048): `apps` gets only what is
    // minted for it. Services (ADR-0049): one that fails at first is
    // restarted; enabled, it starts again at the next boot.
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/packages/heartbeat-1.0.0.opk /keep/heartbeat.opk\r\n",
    b"app install /keep/heartbeat.opk\r\n",
    b"run apps out core:query -- list\r\n",
    b"run apps out core:query -- start app.oceans.hello wait\r\n",
    b"run apps out core:query+run -- start app.oceans.hello wait\r\n",
    b"run apps out core:query+run -- stop app.oceans.hello\r\n",
    b"run apps out core:query+run -- mint query\r\n",
    b"run apps out core:query+run -- mint decide\r\n",
    b"app enable app.oceans.hello\r\n",
    b"app enable app.oceans.heartbeat\r\n",
    b"app info app.oceans.heartbeat\r\n",
    // Go apps (ADR-0052): a signed package whose program is WebAssembly,
    // run by the Go host with the app's permissions (here the console and
    // system information); as a service its output goes to the log.
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/packages/greeter-1.0.0.opk /keep/greeter.opk\r\n",
    b"app install /keep/greeter.opk\r\n",
    b"app info app.oceans.greeter\r\n",
    b"app run app.oceans.greeter alpha beta\r\n",
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/packages/greeter-service-1.0.0.opk /keep/greeter-service.opk\r\n",
    b"app install /keep/greeter-service.opk\r\n",
    b"app enable app.oceans.greeter-service\r\n",
    b"app list\r\n",
    // Oceans AI (ADR-0051): an agent with tools, against the scripted
    // model server; a read-only answer, an action approved, one denied.
    b"ai ask how much memory is free?\r\n",
    b"ai model http://10.0.2.2:$HTTP/v1 oceans-test\r\n",
    b"ai ask how much memory is free?\r\n",
    b"ai ask start the hello app\r\n",
    b"y\r\n",
    b"app list\r\n",
    b"ai ask stop the hello app\r\n",
    b"n\r\n",
    b"app stop app.oceans.hello\r\n",
    // A sensitive read (ADR-0055): only in the folder delegated, asked.
    b"write /home/notes.txt remember the milk\r\n",
    b"ai ask what is in my notes?\r\n",
    b"y\r\n",
    b"ai activity\r\n",
    // The model server by name and over https (ADR-0054): the host's DNS
    // server names it; its certificate's CA is not trusted until `--ca`
    // adds it for this server.
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/models-ca.pem /keep/models-ca.pem\r\n",
    b"ai model https://models.oceans.test:$MODELS/v1 oceans-test --dns 10.0.2.2:$DNS\r\n",
    b"ai ask how much memory is free?\r\n",
    b"ai model https://models.oceans.test:$MODELS/v1 oceans-test --dns 10.0.2.2:$DNS --ca /keep/models-ca.pem\r\n",
    b"ai ask how much memory is free?\r\n",
    // The desktop (ADR-0057): with a mouse plugged in, Hello is clicked in
    // the launcher; it needs a decision, so the desktop's own permission
    // dialog asks, and Allow is clicked. The screen is captured twice for
    // the host to check. Positions are fixed (the layout is anchored at
    // the top left): Hello is the fourth app.
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/packages/notes-1.0.0.opk /keep/notes.opk\r\n",
    b"app install /keep/notes.opk\r\n",
    b"app reset app.oceans.hello network\r\n",
    b"@monitor device_add usb-mouse,bus=usb.0,port=2.3,id=deskmouse",
    b"@monitor screendump build/smoke-desktop.ppm",
    b"@monitor mouse_move -3000 -3000",
    b"@monitor mouse_move 146 234",
    b"@monitor mouse_button 1",
    b"@monitor mouse_button 0",
    b"@monitor screendump build/smoke-dialog.ppm",
    b"@monitor mouse_move 630 108",
    b"@monitor mouse_button 1",
    b"@monitor mouse_button 0",
    // App windows and the keyboard focus (ADR-0059): Notes (the fifth app)
    // is clicked and opens a window, which takes the focus; what the USB
    // keyboard types goes to it, not to the shell, and Enter keeps the
    // line in its storage. Ctrl+Tab gives the keyboard back to the
    // Terminal; the close button ends Notes. The screen is captured with
    // each focus.
    b"@monitor mouse_move -630 -68",
    b"@monitor mouse_button 1",
    b"@monitor mouse_button 0",
    b"@monitor screendump build/smoke-window.ppm",
    b"@keys note\r",
    b"@monitor sendkey ctrl-tab",
    b"@monitor screendump build/smoke-focus.ppm",
    b"@monitor mouse_move 638 -184",
    b"@monitor mouse_button 1",
    b"@monitor mouse_button 0",
    b"@monitor device_del deskmouse",
    b"app info app.oceans.hello\r\n",
    b"cat /apps/app.oceans.notes/data/notes.txt\r\n",
    // The web experience (ADR-0058): the bridge serves nothing but the
    // login until the shell pairs it; the host then uses the System API
    // with the code `ui pair` printed, and loses it with `ui unpair`.
    b"ui status\r\n",
    b"ui pair\r\n",
    b"@bridge paired",
    // The Store (ADR-0061): the browser proposes Tiles; the desktop asks
    // in its own dialog, and Install is clicked with a mouse plugged in
    // again. Nothing is installed before that.
    b"@bridge store",
    b"@monitor device_add usb-mouse,bus=usb.0,port=2.3,id=deskmouse",
    b"@monitor mouse_move -3000 -3000",
    b"@monitor mouse_move 776 342",
    b"@screen 308 220 1b263f the install dialog",
    b"@monitor mouse_button 1",
    b"@monitor mouse_button 0",
    b"@monitor device_del deskmouse",
    b"app list\r\n",
    // A Go app's window (ADR-0060): Tiles, started in the background,
    // shows its first colour; a key typed into it (it has the focus) moves
    // it to the next. Its window is the second opened: 32 pixels further.
    b"app start app.oceans.tiles\r\n",
    b"@screen 509 236 2e7d6b Tiles' first colour",
    b"@monitor sendkey x",
    b"@screen 509 236 c75b39 Tiles' next colour, after a key",
    b"app stop app.oceans.tiles\r\n",
    // Third-party apps built with the SDK (ADR-0062, ADR-0063): refused
    // until the developer's key is trusted at the console; then the Rust
    // app and the Go app install and run.
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/packages/third-party-counter.opk /keep/counter.opk\r\n",
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/packages/third-party-hello.opk /keep/hello-go.opk\r\n",
    b"app install /keep/counter.opk\r\n",
    // Trusted through a date (ADR-0067); a date already past is refused.
    b"app trust add $DEVKEY Example Developer --until 2099-12-31\r\n",
    b"app trust add 1111111111111111111111111111111111111111111111111111111111111111 Oceans Examples\r\n",
    b"app trust add 2222222222222222222222222222222222222222222222222222222222222222 Old Developer --until 2020-01-01\r\n",
    b"app trust\r\n",
    b"app install /keep/counter.opk\r\n",
    // Its first run asks for notifications (ADR-0065), which the user
    // allows: the desktop shows them with the app's name.
    b"app run app.example.counter\r\n",
    b"y\r\n",
    b"app run app.example.counter\r\n",
    b"app install /keep/hello-go.opk\r\n",
    b"app run app.example.hello one two\r\n",
    // A SvelteKit web app (ADR-0064): installed like any app, served by
    // the bridge to the paired browser, never run on Oceans itself.
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/packages/third-party-notes.opk /keep/notes-web.opk\r\n",
    b"app install /keep/notes-web.opk\r\n",
    b"app run app.example.notes\r\n",
    b"@bridge webapp",
    b"ui unpair\r\n",
    b"@bridge unpaired",
    b"ui unpair\r\n",
    // USB (ADR-0032): QEMU's keyboard on its xHCI controller; a command
    // typed on it reaches the shell like any other input.
    b"lsusb\r\n",
    b"run lsusb out use:usb\r\n",
    b"@usb echo typed on usb\r",
    // USB mass storage (ADR-0034) through the block protocol, and as the
    // filesystem mounted at /usb (ADR-0035): formatted when first used,
    // written by the shell and by fetch, synced, then unplugged.
    b"run disk out use:usbdisk -- info\r\n",
    b"ls /usb\r\n",
    b"write /usb/note.txt kept on a usb stick\r\n",
    // Renames (ADR-0038): on the system disk and inside the stick (through
    // the mount); between the two, mv copies and removes (ADR-0039).
    b"write /keep/move-me.txt moved\r\n",
    b"mkdir /keep/sub\r\n",
    b"mv /keep/move-me.txt /keep/sub/moved.txt\r\n",
    b"cat /keep/sub/moved.txt\r\n",
    b"cat /keep/move-me.txt\r\n",
    b"write /usb/tmp.txt temporary\r\n",
    b"mv /usb/tmp.txt /usb/renamed.txt\r\n",
    b"cat /usb/renamed.txt\r\n",
    b"mv /keep/sub/moved.txt /usb/moved.txt\r\n",
    b"cat /keep/sub/moved.txt\r\n",
    // Copies (ADR-0039) through a buffer both filesystems share: a big
    // file and a tree onto the stick, refusals, `rm -r`, and a directory
    // moved back to the system disk.
    b"cp /keep/big.bin /usb/copy.bin\r\n",
    b"mkdir /keep/tree\r\n",
    b"write /keep/tree/a.txt copied tree\r\n",
    b"mkdir /keep/tree/inner\r\n",
    b"cp /keep/tree/a.txt /keep/tree/inner\r\n",
    b"cp /keep/tree /usb/nothing\r\n",
    b"cp -r /keep/tree /keep/tree/inner\r\n",
    b"cp /usb/note.txt /usb\r\n",
    b"cp -r /keep/tree /usb\r\n",
    b"cat /usb/tree/inner/a.txt\r\n",
    b"rm -r /keep/tree\r\n",
    b"ls /keep/tree\r\n",
    b"rm -r /usb\r\n",
    b"mv /usb/tree /keep/tree-back\r\n",
    b"ls /usb/tree\r\n",
    b"cat /usb/note.txt\r\n",
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/big /usb/big.bin\r\n",
    b"sync\r\n",
    b"@monitor device_del stick",
    b"ls /usb\r\n",
    b"run disk out use:usbdisk -- info\r\n",
    // A stick formatted elsewhere: FAT (ADR-0036), read and written at
    // /usb (ADR-0037); the host checks the result, with fsck.fat if it can.
    b"@monitor device_add usb-storage,bus=usb.0,port=4,drive=fatstick,id=fatstick",
    b"ls /usb\r\n",
    b"cat /usb/long-file-name.txt\r\n",
    b"cat /usb/docs/notes/deep.txt\r\n",
    b"write /usb/new.txt written on FAT\r\n",
    b"mkdir /usb/oceans-dir\r\n",
    b"write /usb/oceans-dir/inside.txt nested\r\n",
    b"rm /usb/HELLO.TXT\r\n",
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/big /usb/big.bin\r\n",
    b"sync\r\n",
    b"cat /usb/new.txt\r\n",
    b"cat /usb/oceans-dir/inside.txt\r\n",
    // Renames on FAT: a directory moves (its ".." changes), a file into it.
    b"mkdir /usb/outer\r\n",
    b"mv /usb/oceans-dir /usb/outer/moved-dir\r\n",
    b"mv /usb/new.txt /usb/outer/moved-dir/renamed-on-fat.txt\r\n",
    b"cat /usb/outer/moved-dir/renamed-on-fat.txt\r\n",
    // Copies and a move between FAT and the system disk (ADR-0039).
    b"cp -r /usb/Docs /keep/docs-copy\r\n",
    b"cat /keep/docs-copy/notes/deep.txt\r\n",
    b"mv /keep/tree-back /usb/tree-moved\r\n",
    b"cp /keep/big.bin /usb/outer\r\n",
    b"cat /usb/tree-moved/inner/a.txt\r\n",
    b"sync\r\n",
    // Hubs (ADR-0033) and pointers (ADR-0042): a mouse plugged into the
    // hub appears, is listed and claimed; motion, buttons and the wheel
    // injected through QEMU's monitor reach `mouse` as events. QEMU sends
    // monitor input to the pointer the guest began polling last: the new
    // mouse, and once it is gone the tablet (absolute, buttons only from
    // the monitor). Then the hub goes, taking its tablet along.
    b"@monitor device_add usb-mouse,bus=usb.0,port=2.2,id=hotplug",
    b"run lsusb out use:usb\r\n",
    b"mouse\r\n",
    b"run mouse out use:input -- 4 20\r\n",
    b"@when mouse: waiting for 4 events",
    b"@monitor mouse_move 10 -5",
    b"@monitor mouse_button 1",
    b"@monitor mouse_button 0",
    b"@monitor mouse_move 0 0 1",
    b"@monitor device_del hotplug",
    b"run lsusb out use:usb\r\n",
    b"run mouse out use:input -- 3 20\r\n",
    b"@when mouse: waiting for 3 events",
    b"@monitor mouse_button 2",
    b"@monitor mouse_button 0",
    b"@monitor device_del hub",
    b"run lsusb out use:usb\r\n",
    b"exit\r\n",
];
/// The second smoke boot, on the disk the first one left, with an Intel
/// 82574L in place of the virtio NIC (ADR-0041): the same stack, unchanged,
/// must get its address by DHCP and carry ICMP, UDP, TCP and HTTP over it.
const REBOOT_SCRIPT: &[&[u8]] = &[
    b"app run app.example.counter\r\n",
    b"cat /keep/note.txt\r\n",
    b"ls /keep\r\n",
    b"ls /\r\n",
    b"write /bin/evil x\r\n",
    b"uname\r\n",
    b"ls /usb\r\n",
    b"cat /usb/note.txt\r\n",
    b"cat /nvme/note.txt\r\n",
    b"app list\r\n",
    b"app run app.oceans.hello\r\n",
    b"app run app.oceans.greeter after a reboot\r\n",
    b"app remove app.oceans.hello\r\n",
    b"app disable app.oceans.heartbeat\r\n",
    b"app list\r\n",
    b"run lspci out devices\r\n",
    b"run ifconfig out use:net\r\n",
    b"run ping out use:net -- 10.0.2.2 3\r\n",
    b"run host out use:net -- oceans.test 10.0.2.2:$DNS\r\n",
    b"run nc out use:net -- 10.0.2.2 $TCP hello over e1000e\r\n",
    b"run fetch out use:net -- http://10.0.2.2:$HTTP/hello.txt\r\n",
    b"run fetch out use:net use:fs -- http://10.0.2.2:$HTTP/big /keep/e1000e.bin\r\n",
    // IPv6 over the Intel NIC: its multicast reaches the stack (ADR-0043).
    b"run ping out use:net -- fec0::2 1\r\n",
    b"sync\r\n",
    b"exit\r\n",
];
const REBOOT_EXPECT: &[Expect] = &[
    Expect::Contains("fs (media): mounted the disk"),
    Expect::Line("  usb/"),
    Expect::Line("  big.bin"),
    Expect::Line("kept on a usb stick"),
    Expect::Contains("fs (nvmefs): mounted the disk: generation"),
    Expect::Line("kept on nvme"),
    // Hello, Heartbeat, Greeter, Greeter Service (ADR-0052), Notes
    // (ADR-0059), Tiles (ADR-0060) and the three third-party apps
    // (ADR-0062, ADR-0064), whose developer's key is still trusted
    // (ADR-0063).
    Expect::Contains("core: ready, 9 apps installed, 2 trusted publisher keys"),
    Expect::Line("Counter: run 3"),
    Expect::Contains("core: started service app.oceans.greeter-service"),
    Expect::Contains("greeter: hello from app.oceans.greeter-service 1.0.0, a Go app on Oceans"),
    Expect::Line("greeter: 3 arguments: \"after\" \"a\" \"reboot\""),
    // Its settings, in its own storage (ADR-0053), survived the reboot.
    Expect::Contains("ai: ready; 7 tools; model settings restored"),
    Expect::Line("  app.oceans.hello  1.0.0  Hello"),
    // One run was the desktop's (ADR-0057), two the web experience's
    // (ADR-0058): one started from the browser, one by Oceans AI with the
    // browser's approval.
    Expect::Line("hello: run 11 (counted in my storage)"),
    Expect::Contains("app: removed app.oceans.hello"),
    Expect::Contains("core: started service app.oceans.heartbeat"),
    Expect::Contains("heartbeat: run 3, beating"),
    Expect::Contains("app: disabled app.oceans.heartbeat"),
    Expect::Line("  app.oceans.heartbeat  1.0.0  Heartbeat"),
    Expect::Contains("fs: mounted the disk: generation"),
    Expect::Line("kept across reboots"),
    Expect::Line("  note.txt"),
    Expect::Line("  keep/"),
    Expect::Line("  docs/"),
    Expect::Line("  bin/"),
    Expect::Contains("write: /bin/evil: permission denied"),
    Expect::Contains("Oceans 0.1.0 x86_64 (ABI 15)"),
    // The NIC is an 82574L: virtio-net's device is absent, so init cannot
    // start it, and e1000e's endpoint is the stack's `netdev`.
    Expect::Contains("init: cannot start netdev: "),
    Expect::Contains("e1000e: MAC 52:54:00:12:34:56, 82574L, MSI-X"),
    Expect::Contains("e1000e: link up, 1000 Mb/s full duplex"),
    Expect::Contains("8086:10d3  network  (driver attached)"),
    Expect::Contains("net: configured 10.0.2.15/24 gateway 10.0.2.2 dns 10.0.2.3 (DHCP)"),
    Expect::Contains("net-echo: listening on UDP and TCP port 7"),
    Expect::Line("net0: 10.0.2.15/24 gateway 10.0.2.2 dns 10.0.2.3"),
    Expect::Line("      mac 52:54:00:12:34:56"),
    Expect::Contains("reply from 10.0.2.2: seq=3"),
    Expect::Line("3 sent, 3 received"),
    Expect::Line("oceans.test has address 10.1.2.3"),
    Expect::Line("hello from the host: hello over e1000e"),
    Expect::Contains("reply from fec0::2: seq=1"),
    Expect::Line("hello over http"),
    Expect::Contains("fetch: saved 1048576 bytes"),
];
/// Output the script must produce: `Line` must be a whole console line,
/// `Contains` a substring of one (never text that is also typed input).
const SHELL_EXPECT: &[Expect] = &[
    // Diagnostics (ADR-0070).
    Expect::Contains("diag: needs the logs capability"),
    Expect::Contains(" lines of trouble in the kept log:"),
    Expect::Contains("  [WARN ] process: process init/crasher killed: page fault"),
    Expect::Contains("diag: saved "),
    Expect::Contains("Oceans shell."),
    Expect::Contains("run PROGRAM [GRANT...]"),
    Expect::Line("hello from the shell"),
    Expect::Line("abd"),
    Expect::Contains("use fs"),
    Expect::Line("PING"),
    Expect::Line("  hello-client"),
    Expect::Line("  crasher"),
    Expect::Line("hello filesystem"),
    Expect::Line("  note.txt"),
    Expect::Contains("cat: /docs/note.txt: not found"),
    Expect::Contains("write: /bin/evil: permission denied"),
    Expect::Contains("rm: /bin/crasher: permission denied"),
    Expect::Line("hello-client exited with 0"),
    Expect::Contains("crasher was killed by CPU exception 14"),
    Expect::Contains("run: use:nothing: this shell does not hold it"),
    Expect::Contains("Oceans 0.1.0 x86_64 (ABI 15)"),
    Expect::Contains(" seconds"),
    Expect::Contains("MiB free of"),
    Expect::Contains("PID  PPID  MEMORY"),
    Expect::Contains("init/shell/ps"),
    Expect::Contains("ps: needs the sysinfo capability"),
    Expect::Contains("hello-client: has no manifest"),
    Expect::Contains("run: nosuch: not found"),
    Expect::Contains("frobnicate: unknown command"),
    Expect::Contains("lspci: requests `devices`"),
    Expect::Contains("8086:29c0  host bridge"),
    Expect::Contains("1af4:1042  mass storage  (driver attached)"),
    Expect::Contains("disk: needs the block capability"),
    Expect::Contains("run: use:block: this shell does not hold it"),
    Expect::Contains("fs: formatted a blank disk"),
    Expect::Contains("virtio-net: MAC 52:54:00:12:34:56, MSI-X"),
    Expect::Contains("net: configured 10.0.2.15/24 gateway 10.0.2.2 dns 10.0.2.3 (DHCP)"),
    Expect::Contains("net-echo: listening on UDP and TCP port 7"),
    Expect::Contains("ifconfig: requests `use:net`"),
    Expect::Line("net0: 10.0.2.15/24 gateway 10.0.2.2 dns 10.0.2.3"),
    Expect::Line("      mac 52:54:00:12:34:56"),
    Expect::Contains("reply from 10.0.2.2: seq=1"),
    Expect::Contains("reply from 10.0.2.2: seq=2"),
    Expect::Line("2 sent, 2 received"),
    Expect::Line("timeout: seq=1"),
    Expect::Line("1 sent, 0 received"),
    Expect::Line("oceans.test has address 10.1.2.3"),
    Expect::Line("oceans.test has IPv6 address 2001:db8::1:2:3"),
    Expect::Contains("host: missing.test: not found"),
    Expect::Line("hello from the host: hello over tcp"),
    // IPv6 (ADR-0043): link-local and SLAAC addresses, the router's
    // advertisement, then ICMPv6, UDP (DNS), TCP and HTTP over IPv6.
    Expect::Contains("net: IPv6 fe80::"),
    Expect::Contains("/64 (link-local)"),
    Expect::Contains("net: IPv6 fec0::"),
    Expect::Contains("/64 (SLAAC)"),
    // QEMU advertises its DNS proxy fec0::3 (RDNSS) only when the host
    // itself has an IPv6 DNS server, so " dns fec0::3" may follow.
    Expect::Contains("net: IPv6 router fe80::2"),
    Expect::Contains("      inet6 fe80::"),
    Expect::Contains("/64 link-local"),
    Expect::Contains("      inet6 fec0::"),
    Expect::Contains("/64 autoconf"),
    Expect::Contains("      inet6 router fe80::2"),
    Expect::Line("PING fec0::2 with 57 bytes"),
    Expect::Contains("reply from fec0::2: seq=1"),
    Expect::Contains("reply from fec0::2: seq=2"),
    Expect::Contains("reply from fe80::2: seq=1"),
    Expect::Line("ipv6.oceans.test has IPv6 address 2001:db8::6"),
    Expect::Line("hello from the host over IPv6: hello over ipv6"),
    Expect::Line("hello over http on ipv6"),
    Expect::Line("hello over http"),
    Expect::Line("you were redirected"),
    Expect::Line("chunked transfer works"),
    Expect::Contains("fetch: HTTP 404 Not Found"),
    Expect::Contains("fetch: saved 1048576 bytes"),
    Expect::Contains(" UTC"),
    Expect::Contains("fetch: TLS: invalid peer certificate: UnknownIssuer"),
    Expect::Line("hello over https"),
    Expect::Contains("fetch: TLSv1_3 TLS13_"),
    Expect::Contains("fetch: saved 262144 bytes"),
    Expect::Contains("nvme: QEMU NVMe Ctrl (serial oceans-nvme, firmware "),
    Expect::Contains("nvme: namespace 1: 32768 sectors (16 MiB), 512-byte blocks"),
    Expect::Contains("fs (nvmefs): formatted a blank disk: "),
    Expect::Contains("fs: /nvme: a mounted filesystem"),
    // SATA (ADR-0069): q35's AHCI controller holds the boot disk (the
    // ESP, attached without `if=`); the driver identifies it, and the fs
    // instance on it leaves it untouched (it is not blank).
    Expect::Contains("ahci: AHCI 1.0, 6 ports implemented, 32 command slots, polling"),
    Expect::Contains("ahci: port 0: QEMU HARDDISK (serial QM0000"),
    Expect::Contains(
        "fs (satafs): disk not mounted (it holds something other than an Oceans volume), left untouched",
    ),
    Expect::Contains("fs: /sata: a mounted filesystem"),
    Expect::Line("disk: 32768 sectors of 512 bytes (16 MiB)"),
    Expect::Line("  note.txt"),
    Expect::Line("  big.bin"),
    Expect::Contains("core: ready, 0 apps installed, 1 trusted publisher keys"),
    // Go on Oceans (ADR-0050).
    Expect::Contains("gohello: Go 1."),
    Expect::Contains("gohello: goroutines computed 30"),
    Expect::Contains("gohello: echo replied \"HELLO FROM GO\""),
    Expect::Contains("gohello: System API calls verified"),
    Expect::Contains("ai: ready; 7 tools; no model configured yet"),
    Expect::Contains("framebuffer taken over; desktop ready with "),
    Expect::Contains("app: network for app.oceans.hello will be asked again"),
    Expect::Contains("desktop: permission dialog for app.oceans.hello: network"),
    Expect::Contains("desktop: network allowed for app.oceans.hello in the dialog"),
    Expect::Contains("allowed app.oceans.hello network (in a permission dialog)"),
    Expect::Contains("desktop: started app.oceans.hello"),
    // App windows and the keyboard focus (ADR-0059).
    Expect::Contains("core: apps given `window` now get a window end"),
    Expect::Contains("desktop: started app.oceans.notes"),
    Expect::Contains("display: windows for app.oceans.notes (Notes)"),
    Expect::Contains("core: app.oceans.notes exited with code 0"),
    Expect::Line("note"),
    // Third-party apps built with the SDK (ADR-0062, ADR-0063).
    Expect::Contains("app: /keep/counter.opk: signed with a key this system does not trust"),
    Expect::Contains("core: audit: now trusts key "),
    Expect::Contains(" for publisher Example Developer until 2099-12-31 (added at the console)"),
    Expect::Contains("app: trust: that date has passed"),
    Expect::Contains("added by you, until 2099-12-31"),
    Expect::Contains("desktop: notification: Counter: ran 1 times"),
    Expect::Contains("app: trust: another key is trusted for that publisher name"),
    Expect::Contains("  Oceans Examples  "),
    Expect::Contains("from the system image"),
    Expect::Contains("  Example Developer  "),
    Expect::Contains("added by you"),
    Expect::Line("app: installed app.example.counter 0.1.0"),
    Expect::Line("Counter: hello from app.example.counter 0.1.0, built with the Oceans SDK"),
    Expect::Line("Counter: run 2"),
    Expect::Contains("Counter: "),
    Expect::Line("Hello Go: hello from app.example.hello 0.1.0, built with the Oceans SDK"),
    Expect::Line("Hello Go: 2 arguments"),
    Expect::Line("app: installed app.example.notes 0.1.0"),
    Expect::Contains(
        "app: app.example.notes: a web app: open it from Apps in the Oceans web experience",
    ),
    // A Go app's window (ADR-0060), installed from the Store (ADR-0061).
    Expect::Contains("bridge: the paired browser set the Store's source to http://10.0.2.2:"),
    Expect::Contains(
        "core: audit: proposed installing app.oceans.tiles 1.0.0 from Oceans Examples",
    ),
    Expect::Contains("bridge: proposed installing app.oceans.tiles 1.0.0 from the Store"),
    Expect::Contains("desktop: install dialog for app.oceans.tiles 1.0.0"),
    Expect::Contains("core: audit: install of app.oceans.tiles 1.0.0 confirmed on the device"),
    Expect::Contains("desktop: installed Tiles from the Store"),
    Expect::Line("  app.oceans.tiles  1.0.0  Tiles"),
    Expect::Contains("display: windows for app.oceans.tiles (Tiles)"),
    Expect::Contains("core: audit: stopped app.oceans.tiles"),
    // The web experience (ADR-0058).
    Expect::Contains("bridge: serving the Oceans web experience on TCP port 8080 ("),
    Expect::Line("ui: not paired; listening on port 8080"),
    Expect::Contains("ui: paired. In a browser, open http://10.0.2.15:8080/"),
    Expect::Contains("bridge: paired: a browser presenting the new code may"),
    Expect::Contains("bridge: a pairing code was refused"),
    Expect::Contains("bridge: started app.oceans.hello for the paired browser"),
    Expect::Contains("bridge: stopped app.oceans.hello for the paired browser"),
    Expect::Contains(
        "bridge: AI action approved by the user in the paired browser: start the app Hello",
    ),
    Expect::Line("ui: unpaired; the browser's access is closed"),
    Expect::Contains("bridge: unpaired: the browser's capability is closed"),
    Expect::Line("ui: no browser is paired"),
    Expect::Contains("Oceans AI wants to: read the file notes.txt in your folder"),
    Expect::Contains("Oceans AI: Your notes say: remember the milk"),
    Expect::Contains("files_read {\"path\":\"notes.txt\"} approved by the user: remember the milk"),
    Expect::Contains("ai: model: no model is configured (ai model URL MODEL)"),
    Expect::Contains("ai: using oceans-test at http://10.0.2.2:"),
    Expect::Contains("Oceans AI: Memory: "),
    Expect::Contains(
        "Oceans AI wants to: start the app Hello (app.oceans.hello) with the arguments \"wait\"",
    ),
    Expect::Contains("Oceans AI: Done: started Hello (app.oceans.hello)"),
    Expect::Line("  app.oceans.hello  1.0.0  Hello  (running)"),
    Expect::Contains("Oceans AI wants to: stop the app Hello (app.oceans.hello)"),
    Expect::Contains("Oceans AI: Understood: I left Hello running."),
    Expect::Contains("system_memory {} (read-only): "),
    Expect::Contains(
        "apps_start {\"id\":\"app.oceans.hello\",\"args\":\"wait\"} approved by the user: started Hello",
    ),
    Expect::Contains("apps_stop {\"id\":\"app.oceans.hello\"} denied by the user"),
    // The gateway by name and over https (ADR-0054).
    Expect::Contains("ai: using oceans-test at https://models.oceans.test:"),
    Expect::Contains(" root certificates from ca-roots.pem"),
    Expect::Contains("x509: certificate signed by unknown authority"),
    Expect::Contains("ai: trusting 1 more CA certificates for models.oceans.test"),
    Expect::Contains("ai: model server: TLS 1.3 with models.oceans.test in "),
    Expect::Contains("Oceans AI: Memory over https: "),
    Expect::Contains("app: /keep/untrusted.opk: signed with a key this system does not trust"),
    Expect::Contains(
        "app: /keep/tampered.opk: the signature does not match: the package was changed",
    ),
    Expect::Contains("app: installed app.oceans.hello 1.0.0"),
    Expect::Line("  app.oceans.hello  1.0.0  Hello"),
    Expect::Contains("Hello (app.oceans.hello 1.0.0, from Oceans Examples) asks to:"),
    Expect::Line("  connect to the internet and the local network"),
    Expect::Contains("reason given by the app: \"say hello to the server you name\""),
    Expect::Contains("app: network denied"),
    Expect::Line("Hello from app.oceans.hello 1.0.0"),
    Expect::Line("hello: run 1 (counted in my storage)"),
    Expect::Line("hello: no network permission; not connecting"),
    Expect::Contains("app: network allowed for app.oceans.hello"),
    Expect::Line("hello: run 2 (counted in my storage)"),
    Expect::Line("hello from the host: hello from an app"),
    Expect::Contains("network      allowed     say hello to the server you name"),
    Expect::Contains("app: updated app.oceans.hello 1.0.0 -> 2.0.0"),
    Expect::Contains("app: /keep/hello.opk: not newer than the installed version"),
    Expect::Line("Hello from app.oceans.hello 2.0.0"),
    Expect::Line("hello: run 3 (counted in my storage)"),
    Expect::Contains("app: started app.oceans.hello"),
    Expect::Line("hello: waiting until stopped"),
    Expect::Line("  app.oceans.hello  2.0.0  Hello  (running)"),
    Expect::Contains(
        "app: network revoked for app.oceans.hello; it was running and has been stopped",
    ),
    Expect::Contains("app: app.oceans.hello rolled back to 1.0.0"),
    Expect::Contains("app: stopped app.oceans.hello"),
    Expect::Contains("installed app.oceans.hello 1.0.0 from Oceans Examples"),
    Expect::Contains("denied app.oceans.hello network (at its prompt)"),
    Expect::Contains("stopped app.oceans.hello: a permission it used was revoked"),
    Expect::Contains("app: installed app.oceans.heartbeat 1.0.0"),
    Expect::Line("app.oceans.heartbeat 1.0.0"),
    Expect::Line("app.oceans.hello 1.0.0"),
    Expect::Contains("apps: app.oceans.hello: not allowed by this capability"),
    Expect::Contains("apps: started app.oceans.hello"),
    Expect::Contains("apps: stopped app.oceans.hello"),
    Expect::Contains("apps: minted query"),
    Expect::Contains("apps: decide: not allowed by this capability"),
    Expect::Contains("app: app.oceans.hello: not a service (only services start at boot)"),
    Expect::Contains("app: enabled app.oceans.heartbeat: it runs now and at every boot"),
    Expect::Contains("heartbeat: run 1, failing on purpose"),
    Expect::Contains(
        "core: service app.oceans.heartbeat failed (exit 3); restarting in 1 s (1 of 5)",
    ),
    Expect::Contains("core: restarted service app.oceans.heartbeat"),
    Expect::Contains("heartbeat: run 2, beating"),
    Expect::Contains("service, enabled (starts at boot), "),
    Expect::Contains("app: installed app.oceans.greeter 1.0.0"),
    Expect::Line("  runtime: wasm"),
    Expect::Line("Greeter (app.oceans.greeter) 1.0.0"),
    Expect::Line("greeter: hello from app.oceans.greeter 1.0.0, a Go app on Oceans"),
    Expect::Line("greeter: 2 arguments: \"alpha\" \"beta\""),
    Expect::Contains("greeter: memory: "),
    Expect::Contains("core: app.oceans.greeter exited with code 0"),
    Expect::Contains("started app.oceans.greeter 1.0.0 with console, system-info"),
    Expect::Contains("app: installed app.oceans.greeter-service 1.0.0"),
    Expect::Contains("greeter: hello from app.oceans.greeter-service 1.0.0, a Go app on Oceans"),
    Expect::Contains("greeter: no arguments"),
    Expect::Contains("core: app.oceans.greeter-service exited with code 0"),
    Expect::Line("  app.oceans.greeter  1.0.0  Greeter"),
    Expect::Contains("lsusb: requests `use:usb`"),
    Expect::Contains(
        "xhci: port 5: 0627:0001 QEMU USB Keyboard (480 Mb/s), keyboard (console input)",
    ),
    Expect::Line("port 5: 0627:0001 QEMU USB Keyboard (480 Mb/s) keyboard (console input)"),
    Expect::Line("typed on usb"),
    Expect::Contains("xhci: port 6: 0409:55aa QEMU USB Hub (12 Mb/s), hub"),
    Expect::Contains("xhci: port 3: interface 0 handed to a class driver"),
    Expect::Contains("usb-storage: port 3: QEMU QEMU HARDDISK, 4 MiB (8192 blocks of 512 bytes)"),
    Expect::Line("port 3: 46f4:0001 QEMU USB HARDDRIVE (5 Gb/s) mass storage"),
    Expect::Line("disk: 8192 sectors of 512 bytes (4 MiB)"),
    Expect::Contains("fs: /usb: a mounted filesystem"),
    Expect::Contains("fs (media): ready for removable media"),
    Expect::Contains("fs (media): formatted a blank disk: "),
    Expect::Line("kept on a usb stick"),
    Expect::Contains("fs (media): unmounted the disk (disk removed)"),
    Expect::Contains("ls: /usb: no disk"),
    Expect::Contains("fs (media): mounted a FAT16 volume \"OCEANS16\", read-write"),
    Expect::Line("  HELLO.TXT"),
    Expect::Line("  A long file name.txt"),
    Expect::Line("  ไฟล์ภาษาไทย.txt"),
    Expect::Line("  Docs/"),
    Expect::Line("long names work"),
    Expect::Line("deep"),
    Expect::Line("written on FAT"),
    Expect::Line("nested"),
    Expect::Line("moved"),
    Expect::Contains("cat: /keep/move-me.txt: not found"),
    Expect::Line("temporary"),
    Expect::Contains("cat: /keep/sub/moved.txt: not found"),
    Expect::Contains("cp: 1048576 bytes in 1 file, "),
    Expect::Contains("cp: /keep/tree: is a directory (cp -r copies directories)"),
    Expect::Contains("cp: /keep/tree/inner: is inside the source"),
    Expect::Contains("cp: /usb: is the same file"),
    Expect::Contains("cp: 24 bytes in 2 files, "),
    Expect::Line("copied tree"),
    Expect::Contains("ls: /keep/tree: not found"),
    Expect::Contains("rm: /usb: permission denied"),
    Expect::Contains("ls: /usb/tree: not found"),
    Expect::Contains("cp: 5 bytes in 1 file, "),
    Expect::Contains("usb-storage: port 3: disk removed"),
    Expect::Contains("xhci: port 3: device removed"),
    Expect::Contains("disk: I/O error"),
    Expect::Contains("xhci: port 6.1: 0627:0001 QEMU USB Tablet (12 Mb/s), tablet"),
    Expect::Contains("usb-hid: port 6.1: tablet, "),
    Expect::Contains(", 0..32767 x 0..32767, pointer 1"),
    Expect::Line("port 6.1: 0627:0001 QEMU USB Tablet (12 Mb/s) tablet (pointer input)"),
    Expect::Contains("usb-hid: port 6.2: mouse, boot protocol, pointer 4"),
    Expect::Line("port 6.2: 0627:0001 QEMU USB Mouse (12 Mb/s) mouse (pointer input)"),
    Expect::Contains("mouse: requests `use:input`"),
    Expect::Contains("pointer 4: motion dx=10 dy=-5"),
    Expect::Contains("pointer 4: button 1 (left) down"),
    Expect::Contains("pointer 4: button 1 (left) up"),
    Expect::Contains("pointer 4: wheel vertical=1 horizontal=0"),
    Expect::Contains("usb-hid: port 6.2: pointer 4 removed"),
    Expect::Contains("pointer 1: absolute x=0 y=0 (of 32767 x 32767)"),
    Expect::Contains("pointer 1: button 2 (right) down"),
    Expect::Contains("pointer 1: button 2 (right) up"),
    Expect::Contains("xhci: port 6.2: device removed"),
    Expect::Contains("xhci: port 6.1: device removed"),
    Expect::Contains("xhci: port 6: device removed"),
];

#[derive(Clone, Copy, Debug)]
enum Expect {
    Line(&'static str),
    Contains(&'static str),
}

impl Expect {
    fn matches(self, line: &str) -> bool {
        // The console echoes the prompt and typed input; strip it so only
        // command output can satisfy a `Line`.
        let output = line.trim_end_matches('\r');
        match self {
            Self::Line(expected) => output == expected,
            Self::Contains(expected) => output.contains(expected),
        }
    }
}
/// Each smoke boot, start to finish. Boot 1 runs ~150 scripted steps, many
/// with QEMU monitor pauses; CI runners without KVM are slow.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(480);
/// How long the last command waits for the host's echo probes.
const PROBE_WAIT: Duration = Duration::from_secs(30);
/// QEMU exit status for `EmulatorExit::Success` (0x10 << 1 | 1).
const QEMU_EXIT_SUCCESS: i32 = 33;

type Result<T = ()> = std::result::Result<T, String>;

const USAGE: &str = "\
usage: cargo xtask <command> [--release]

commands:
  check     format check, clippy (host + kernel) and host unit tests
  build     build the kernel
  limine    download the Limine UEFI bootloader into build/limine
  image     build the kernel and assemble the EFI system partition in build/esp
  run       boot Oceans in QEMU with serial output on this terminal
  smoke     boot Oceans headless in QEMU and verify it comes online
  usb       build/oceans-usb.img: the image a real machine boots from (ADR-0068)
  smoke-hw  boot that image in QEMU as a real PC would (no virtio)
  release   build/release: a release's USB image, update and checksums,
            trusting only the release key (ADR-0072)

environment:
  OCEANS_QEMU   path to qemu-system-x86_64
  OCEANS_OVMF   path to the x86_64 UEFI firmware code image (OVMF/edk2)
  OCEANS_LIMINE directory containing BOOTX64.EFI (default: build/limine)
  OCEANS_QEMU_EXTRA extra QEMU arguments, e.g. \"-cpu max\"
  OCEANS_NIC    the network card for `run`: virtio (default) or e1000e
  OCEANS_BUN    path to bun, which builds the web experience (ui/)
  OCEANS_BRIDGE_PORT the host port `run` forwards to the web experience (8080)
  OCEANS_RELEASE_KEY the release key file for `release` (`oceans keygen`;
                kept outside the repository)";

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let command = args.next();
    let rest: Vec<String> = args.collect();
    let profile = if rest.iter().any(|a| a == "--release") {
        Profile::Release
    } else {
        Profile::Dev
    };
    if let Some(unknown) = rest.iter().find(|a| *a != "--release") {
        eprintln!("unknown argument: {unknown}\n\n{USAGE}");
        return ExitCode::from(2);
    }

    let result = match command.as_deref() {
        Some("check") => check(),
        Some("build") => build_kernel(profile).map(drop),
        Some("limine") => fetch_limine(),
        Some("image") => build_image(profile, None).map(drop),
        Some("run") => run(profile),
        Some("smoke") => smoke(profile),
        Some("usb") => hardware::usb(profile),
        Some("smoke-hw") => hardware::smoke_hw(profile),
        Some("release") => release::release(),
        Some("help" | "--help" | "-h") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: error: {message}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy)]
enum Profile {
    Dev,
    Release,
}

impl Profile {
    fn dir(self) -> &'static str {
        match self {
            Self::Dev => "debug",
            Self::Release => "release",
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("xtask lives at tools/xtask")
        .to_path_buf()
}

fn cargo() -> Command {
    let mut cmd = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cmd.current_dir(root());
    cmd
}

fn run_command(cmd: &mut Command) -> Result {
    let status = cmd
        .status()
        .map_err(|e| format!("failed to start {:?}: {e}", cmd.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed with {status}", cmd.get_program()))
    }
}

fn check() -> Result {
    run_command(cargo().args(["fmt", "--all", "--check"]))?;
    run_command(cargo().args([
        "clippy",
        "--workspace",
        "--exclude",
        KERNEL_PACKAGE,
        "--",
        "-D",
        "warnings",
    ]))?;
    run_command(cargo().args([
        "clippy",
        "-p",
        KERNEL_PACKAGE,
        "--target",
        KERNEL_TARGET,
        "--",
        "-D",
        "warnings",
    ]))?;
    run_command(cargo().args(["test", "--workspace", "--exclude", KERNEL_PACKAGE]))?;
    run_command(user_cargo().args(["fmt", "--all", "--check"]))?;
    run_command(user_cargo().args(["clippy", "--release", "--", "-D", "warnings"]))?;
    check_ui()?;
    check_go()
}

/// Cargo in the `user/` workspace, whose `.cargo/config.toml` selects the
/// target and the user code model.
fn user_cargo() -> Command {
    let mut cmd = cargo();
    cmd.current_dir(root().join("user"));
    cmd
}

/// Builds the user programs; returns the directory holding the binaries.
/// Always release builds: they are shipped system programs, loaded through
/// the filesystem, so size matters (ADR-0020); the kernel profile does not
/// change them.
fn build_user() -> Result<PathBuf> {
    run_command(user_cargo().args(["build", "--release"]))?;
    Ok(root()
        .join("user")
        .join("target")
        .join(KERNEL_TARGET)
        .join(Profile::Release.dir()))
}

fn build_kernel(profile: Profile) -> Result<PathBuf> {
    let mut cmd = cargo();
    cmd.args(["build", "-p", KERNEL_PACKAGE, "--target", KERNEL_TARGET]);
    if let Profile::Release = profile {
        cmd.arg("--release");
    }
    run_command(&mut cmd)?;
    Ok(root()
        .join("target")
        .join(KERNEL_TARGET)
        .join(profile.dir())
        .join(KERNEL_PACKAGE))
}

fn limine_dir() -> PathBuf {
    env::var_os("OCEANS_LIMINE").map_or_else(|| root().join("build").join("limine"), PathBuf::from)
}

fn fetch_limine() -> Result {
    let dir = limine_dir();
    if dir.join("BOOTX64.EFI").is_file() {
        println!("Limine already present in {}", dir.display());
        return Ok(());
    }
    println!(
        "cloning Limine {LIMINE_BRANCH} from {LIMINE_REPO} into {}",
        dir.display()
    );
    run_command(
        Command::new("git")
            .args(["clone", "--depth=1", "--branch", LIMINE_BRANCH, LIMINE_REPO])
            .arg(&dir),
    )
}

/// Builds `build/esp`, a directory QEMU exposes as a FAT drive:
///
/// ```text
/// EFI/BOOT/BOOTX64.EFI        Limine
/// boot/limine/limine.conf
/// boot/oceans-kernel
/// boot/initrd                 the boot archive: every program and the
///                             service manifest (ADR-0025)
/// ```
/// Which services an image boots.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Setup {
    /// `config/services.conf`: QEMU and development.
    Normal,
    /// `config/services-smoke.conf`, with the smoke test's command line.
    Smoke,
    /// The hardware profile (ADR-0068), made from `services.conf`.
    Hardware,
}

fn build_image(profile: Profile, cmdline: Option<&str>) -> Result<PathBuf> {
    let setup = if cmdline.is_some() {
        Setup::Smoke
    } else {
        Setup::Normal
    };
    build_image_for(profile, cmdline, setup, &release::ImageKeys::development()?)
}

fn build_image_for(
    profile: Profile,
    cmdline: Option<&str>,
    setup: Setup,
    keys: &release::ImageKeys,
) -> Result<PathBuf> {
    let kernel = build_kernel(profile)?;
    let user = build_user()?;

    let limine_efi = limine_dir().join("BOOTX64.EFI");
    if !limine_efi.is_file() {
        return Err(format!(
            "{} not found; run `cargo xtask limine` or set OCEANS_LIMINE",
            limine_efi.display()
        ));
    }

    let esp = root().join("build").join("esp");
    if esp.exists() {
        fs::remove_dir_all(&esp).map_err(|e| format!("cannot clear {}: {e}", esp.display()))?;
    }
    let efi_boot = esp.join("EFI").join("BOOT");
    let limine_conf_dir = esp.join("boot").join("limine");
    for dir in [&efi_boot, &limine_conf_dir] {
        fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }

    copy(&limine_efi, &efi_boot.join("BOOTX64.EFI"))?;
    copy(&kernel, &esp.join("boot").join(KERNEL_PACKAGE))?;
    let manifest = if setup == Setup::Smoke {
        SMOKE_MANIFEST
    } else {
        MANIFEST
    };
    let mut files = Vec::new();
    for program in USER_PROGRAMS {
        let path = user.join(program);
        let bytes = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        files.push((program.to_string(), bytes));
    }
    let manifest_path = root().join(manifest);
    let mut manifest_bytes = fs::read(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    if setup == Setup::Hardware {
        let text = String::from_utf8(manifest_bytes).map_err(|_| "services.conf is not UTF-8")?;
        manifest_bytes = hardware::hardware_services(&text)?.into_bytes();
    }
    files.push(("services.conf".to_string(), manifest_bytes));
    build_ui()?;
    for (name, bytes) in build_go(GO_PROGRAMS)? {
        files.push((name, bytes));
    }
    // Oceans Core's trusted publisher keys (ADR-0046): the root of trust
    // for apps comes with the boot image.
    files.push(("trust.keys".to_string(), keys.trust_list().into_bytes()));
    // The release, and the keys system updates are accepted from
    // (ADR-0071): `update` reads them as /bin/release and /bin/update.keys.
    files.push((
        "release".to_string(),
        format!("{RELEASE_VERSION} {RELEASE_CHANNEL}\n").into_bytes(),
    ));
    files.push(("update.keys".to_string(), keys.update_keys().into_bytes()));
    // The trusted roots for TLS in Go services (ADR-0054): the AI's model
    // gateway gets them as a module.
    files.push(("ca-roots.pem".to_string(), ca_roots_pem()?.into_bytes()));
    build_packages(&user, &build_go(GO_APPS)?)?;
    let entries: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
        .collect();
    let mut archive = vec![0u8; oceans_archive::archive_len(&entries)];
    oceans_archive::write(&entries, &mut archive)
        .map_err(|e| format!("cannot build the boot archive: {e:?}"))?;
    let archive_path = esp.join("boot").join("initrd");
    fs::write(&archive_path, &archive)
        .map_err(|e| format!("cannot write {}: {e}", archive_path.display()))?;

    if setup == Setup::Hardware {
        // Two slots for updates (ADR-0071).
        hardware::slot_layout(&esp, &format!("{RELEASE_VERSION} {RELEASE_CHANNEL}"))?;
        println!(
            "image ready in {} (boot archive: {} files, {} KiB; slot a)",
            esp.display(),
            entries.len(),
            archive.len() / 1024
        );
        return Ok(esp);
    }
    let mut conf = String::from("timeout: 0\n\n/Oceans\n    protocol: limine\n");
    conf.push_str(&format!("    path: boot():/boot/{KERNEL_PACKAGE}\n"));
    if let Some(cmdline) = cmdline {
        conf.push_str(&format!("    cmdline: {cmdline}\n"));
    }
    conf.push_str("    module_path: boot():/boot/initrd\n");
    let conf_path = limine_conf_dir.join("limine.conf");
    fs::write(&conf_path, conf)
        .map_err(|e| format!("cannot write {}: {e}", conf_path.display()))?;

    println!(
        "image ready in {} (boot archive: {} files, {} KiB)",
        esp.display(),
        entries.len(),
        archive.len() / 1024
    );
    pack_esp(&esp)?;
    Ok(esp)
}

/// The ESP as a FAT image QEMU boots from (`build/esp.img`).
const ESP_IMAGE: &str = "build/esp.img";

/// Packs `esp` into [`ESP_IMAGE`] with mkfs.fat and mtools when they can be
/// found (on the PATH, or in WSL below `OCEANS_FAT_TOOLS_WSL`). Without
/// them, QEMU serves the directory itself (vvfat), which older QEMU (8.2,
/// as on CI) can misread once the guest has written to it.
fn pack_esp(esp: &Path) -> Result {
    let image = root().join(ESP_IMAGE);
    if image.exists() {
        fs::remove_file(&image).map_err(|e| format!("cannot remove {}: {e}", image.display()))?;
    }
    let mut used = 0u64;
    let mut stack = vec![esp.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let meta = entry.metadata().map_err(|e| e.to_string())?;
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                used += meta.len();
            }
        }
    }
    // Room for the firmware's variables and growth; FAT32 at 64 MiB or more.
    let kib = (used / 1024 * 5 / 4 + 8 * 1024).max(64 * 1024);
    let mkfs = fat_tool("usr/sbin/mkfs.fat")
        .args(["-C", "-F", "32", "-n", "OCEANS-ESP"])
        .arg(host_path_for_tool(&image))
        .arg(kib.to_string())
        .output();
    match mkfs {
        Ok(output) if output.status.success() => {}
        // Not installed: fall back to vvfat.
        Ok(output) if output.status.code() == Some(127) => return Ok(()),
        Err(_) => return Ok(()),
        Ok(output) => {
            return Err(format!(
                "mkfs.fat failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    let output = fat_tool("usr/bin/mcopy")
        .args(["-s", "-i"])
        .arg(host_path_for_tool(&image))
        .arg(host_path_for_tool(&esp.join("EFI")))
        .arg(host_path_for_tool(&esp.join("boot")))
        .arg("::/")
        .output()
        .map_err(|e| format!("cannot run mcopy: {e}"))?;
    if !output.status.success() {
        let _ = fs::remove_file(&image);
        return Err(format!(
            "mcopy failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    println!("ESP packed into {ESP_IMAGE} ({} MiB)", kib / 1024);
    Ok(())
}

/// A FAT tool (`relative` below the tools root, as in Debian's packages):
/// in WSL below `OCEANS_FAT_TOOLS_WSL` if set, else from the PATH.
fn fat_tool(relative: &str) -> Command {
    let name = relative.rsplit('/').next().unwrap_or(relative);
    match env::var("OCEANS_FAT_TOOLS_WSL") {
        Ok(tools) => {
            let mut cmd = Command::new("wsl");
            cmd.args([
                "-e",
                "env",
                "MTOOLS_SKIP_CHECK=1",
                &format!("{tools}/{relative}"),
            ]);
            cmd
        }
        Err(_) => {
            let mut cmd = Command::new(name);
            cmd.env("MTOOLS_SKIP_CHECK", "1");
            cmd
        }
    }
}

/// `path` as the FAT tools see it: a WSL path when they run in WSL.
fn host_path_for_tool(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if env::var_os("OCEANS_FAT_TOOLS_WSL").is_none() {
        return text;
    }
    let text = text.strip_prefix("//?/").unwrap_or(&text).to_string();
    match text.split_once(":/") {
        Some((drive, rest)) => format!("/mnt/{}/{rest}", drive.to_ascii_lowercase()),
        None => text,
    }
}

fn copy(from: &Path, to: &Path) -> Result {
    fs::copy(from, to)
        .map(drop)
        .map_err(|e| format!("cannot copy {} to {}: {e}", from.display(), to.display()))
}

/// Creates a blank (zero-filled) disk image, which the filesystem formats
/// on first use; an existing one is replaced only if `fresh`.
fn prepare_disk(path: &str, fresh: bool) -> Result {
    let path = root().join(path);
    if path.is_file() && !fresh {
        return Ok(());
    }
    fs::write(&path, vec![0u8; DISK_SIZE as usize])
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The MAC QEMU gives the guest's network device.
const GUEST_MAC: &str = "52:54:00:12:34:56";

/// The guest's network card (ADR-0041): whichever is present, its driver
/// serves the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Nic {
    /// Modern-only virtio-net (`1af4:1041`), driven by virtio-net.
    Virtio,
    /// An Intel 82574L (`8086:10d3`), driven by e1000e.
    E1000e,
}

impl Nic {
    /// `OCEANS_NIC`, for `run`.
    fn from_env() -> Result<Self> {
        match env::var("OCEANS_NIC").as_deref() {
            Err(_) | Ok("" | "virtio") => Ok(Self::Virtio),
            Ok("e1000e") => Ok(Self::E1000e),
            Ok(other) => Err(format!("OCEANS_NIC={other}: expected `virtio` or `e1000e`")),
        }
    }

    fn device(self) -> String {
        match self {
            Self::Virtio => format!("virtio-net-pci,netdev=net0,disable-legacy=on,mac={GUEST_MAC}"),
            Self::E1000e => format!("e1000e,netdev=net0,mac={GUEST_MAC}"),
        }
    }
}

/// The disk images a boot attaches.
#[derive(Clone, Copy)]
struct Images<'a> {
    /// The virtio system disk.
    disk: &'a str,
    nvme: &'a str,
    /// The USB stick plugged in at boot.
    stick: &'a str,
    /// An image QEMU knows but has not plugged in (the monitor plugs it in
    /// as `fatstick`).
    spare_stick: Option<&'a str>,
}

/// `forward`: host (UDP, TCP) ports forwarded to the guest's port 7;
/// `bridge`: a host port forwarded to the bridge's (ADR-0058); `monitor`:
/// a host port for QEMU's monitor (to press USB keys); `nic`: the network
/// card.
fn qemu_command(
    headless: bool,
    nic: Nic,
    images: &Images<'_>,
    forward: Option<(u16, u16)>,
    bridge: Option<(u16, u16)>,
    monitor: Option<u16>,
) -> Result<Command> {
    let Images {
        disk,
        nvme,
        stick,
        spare_stick,
    } = *images;
    let qemu = find_qemu()?;
    let firmware = find_firmware(&qemu)?;

    let mut cmd = Command::new(&qemu);
    cmd.current_dir(root());
    let mut pflash = OsString::from("if=pflash,format=raw,readonly=on,file=");
    pflash.push(&firmware);
    cmd.args([
        "-machine",
        "q35",
        "-m",
        "256M",
        "-no-reboot",
        "-serial",
        "stdio",
    ])
    .arg("-drive")
    .arg(pflash)
    // Relative path: QEMU's option parser would split an absolute
    // Windows path at the drive-letter colon. `rw:` because QEMU refuses a
    // read-only vvfat node on a writable IDE disk (and refuses `snapshot`
    // with `rw`). vvfat writes guest changes back to this directory: the
    // firmware stores its variables there (NvVars), and the write-back has
    // rewritten the boot loader itself, so callers rebuild the image before
    // every boot. A packed image (`pack_esp`) replaces it when the FAT
    // tools are there.
    .args([
        "-drive",
        if root().join(ESP_IMAGE).is_file() {
            "format=raw,file=build/esp.img"
        } else {
            "format=raw,file=fat:rw:build/esp"
        },
    ])
    // A modern-only virtio disk (PCI ID 1af4:1042), driven by the
    // userspace virtio-blk service.
    .arg("-drive")
    .arg(format!("if=none,id=disk0,format=raw,file={disk}"))
    .args(["-device", "virtio-blk-pci,drive=disk0,disable-legacy=on"])
    // An NVMe controller (class 010802) with one namespace, driven by the
    // userspace nvme service (ADR-0040).
    .arg("-drive")
    .arg(format!("if=none,id=nvme0,format=raw,file={nvme}"))
    .args(["-device", "nvme,serial=oceans-nvme,drive=nvme0"])
    // The network card on QEMU's user network (NAT, DHCP at 10.0.2.2;
    // IPv6 router advertisements for fec0::/64 from fe80::2, ADR-0043): a
    // modern-only virtio NIC or an Intel 82574L, each driven by its
    // userspace driver (ADR-0023, ADR-0041). Both address families are
    // named: naming only one turns the other off.
    .arg("-netdev")
    .arg({
        let mut netdev = "user,id=net0,ipv4=on,ipv6=on".to_string();
        if let Some((udp, tcp)) = forward {
            netdev.push_str(&format!(
                ",hostfwd=udp:127.0.0.1:{udp}-:7,hostfwd=tcp:127.0.0.1:{tcp}-:7"
            ));
        }
        if let Some((port, apps)) = bridge {
            netdev.push_str(&format!(
                ",hostfwd=tcp:127.0.0.1:{port}-:{BRIDGE_PORT},hostfwd=tcp:127.0.0.1:{apps}-:{BRIDGE_APP_PORT}"
            ));
        }
        netdev
    })
    .arg("-device")
    .arg(nic.device())
    // A USB 3 host controller (class 0c0330) with a keyboard, driven by the
    // userspace xhci service (ADR-0032).
    .args([
        "-device",
        "qemu-xhci,id=usb",
        "-device",
        "usb-kbd,bus=usb.0,port=1",
    ])
    // A USB 3 stick (ADR-0034); QEMU attaches it to xHCI port 3.
    .arg("-drive")
    .arg(format!("if=none,id=stick,format=raw,file={stick}"))
    .args([
        "-device",
        "usb-storage,bus=usb.0,port=3,drive=stick,id=stick",
    ])
    // A hub with a tablet behind it (ADR-0033).
    .args(["-device", "usb-hub,bus=usb.0,port=2,id=hub"])
    .args(["-device", "usb-tablet,bus=usb.0,port=2.1"])
    .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"]);
    if let Some(spare) = spare_stick {
        cmd.arg("-drive")
            .arg(format!("if=none,id=fatstick,format=raw,file={spare}"));
    }
    if let Some(port) = monitor {
        cmd.arg("-monitor")
            .arg(format!("tcp:127.0.0.1:{port},server=on,wait=off"));
    }
    if headless {
        cmd.args(["-display", "none"]);
    }
    // e.g. OCEANS_QEMU_EXTRA="-cpu max" to exercise SMEP/SMAP/UMIP.
    if let Some(extra) = env::var_os("OCEANS_QEMU_EXTRA") {
        cmd.args(extra.to_string_lossy().split_whitespace());
    }
    Ok(cmd)
}

fn find_qemu() -> Result<PathBuf> {
    if let Some(path) = env::var_os("OCEANS_QEMU") {
        return Ok(PathBuf::from(path));
    }
    let exe = if cfg!(windows) {
        "qemu-system-x86_64.exe"
    } else {
        "qemu-system-x86_64"
    };
    let from_path = env::var_os("PATH")
        .map(|paths| {
            env::split_paths(&paths)
                .map(|dir| dir.join(exe))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let well_known = [PathBuf::from(r"C:\Program Files\qemu").join(exe)];
    from_path
        .into_iter()
        .chain(well_known)
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| "qemu-system-x86_64 not found; install QEMU or set OCEANS_QEMU".to_string())
}

fn find_firmware(qemu: &Path) -> Result<PathBuf> {
    if let Some(path) = env::var_os("OCEANS_OVMF") {
        return Ok(PathBuf::from(path));
    }
    let mut candidates = Vec::new();
    if let Some(qemu_dir) = qemu.parent() {
        // Windows installer layout and Homebrew-style prefix/share/qemu.
        candidates.push(qemu_dir.join("share").join("edk2-x86_64-code.fd"));
        if let Some(prefix) = qemu_dir.parent() {
            candidates.push(
                prefix
                    .join("share")
                    .join("qemu")
                    .join("edk2-x86_64-code.fd"),
            );
        }
    }
    candidates.extend(
        [
            "/usr/share/OVMF/OVMF_CODE_4M.fd",
            "/usr/share/OVMF/OVMF_CODE.fd",
            "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
            "/usr/share/edk2/ovmf/OVMF_CODE.fd",
            "/usr/share/qemu/edk2-x86_64-code.fd",
        ]
        .map(PathBuf::from),
    );
    candidates.into_iter().find(|c| c.is_file()).ok_or_else(|| {
        "x86_64 UEFI firmware (OVMF/edk2) not found; install it or set OCEANS_OVMF".to_string()
    })
}

fn run(profile: Profile) -> Result {
    build_image(profile, None)?;
    prepare_disk(DISK_IMAGE, false)?;
    prepare_blank(NVME_IMAGE, NVME_SIZE, false)?;
    prepare_stick(STICK_IMAGE, false)?;
    // The web experience (ADR-0058), from this machine's browser.
    let bridge = match env::var("OCEANS_BRIDGE_PORT") {
        Ok(port) => port
            .parse::<u16>()
            .map_err(|_| format!("OCEANS_BRIDGE_PORT={port}: expected a port number"))?,
        Err(_) => BRIDGE_PORT,
    };
    println!("the web experience: http://127.0.0.1:{bridge}/ (type `ui pair` in the shell)");
    let apps = bridge
        .checked_add(1)
        .ok_or("OCEANS_BRIDGE_PORT is too high")?;
    println!("web apps (ADR-0064): from http://127.0.0.1:{apps}/");
    run_command(&mut qemu_command(
        false,
        Nic::from_env()?,
        &Images {
            disk: DISK_IMAGE,
            nvme: NVME_IMAGE,
            stick: STICK_IMAGE,
            spare_stick: None,
        },
        None,
        Some((bridge, apps)),
        None,
    )?)
}

/// The FAT stick: the fixture, expanded (`OCSPARSE`: size, then runs of
/// offset, length, bytes; see libs/fat/testdata/sparse.py).
fn prepare_fat_stick() -> Result {
    let blob = FAT_FIXTURE;
    let u64_at = |at: usize| u64::from_le_bytes(blob[at..at + 8].try_into().expect("8 bytes"));
    if !blob.starts_with(b"OCSPARSE") {
        return Err("the FAT fixture is not a sparse image".into());
    }
    let mut image = vec![0u8; u64_at(8) as usize];
    let mut at = 16;
    while at < blob.len() {
        let offset = u64_at(at) as usize;
        let len = u32::from_le_bytes(blob[at + 8..at + 12].try_into().expect("4 bytes")) as usize;
        at += 12;
        image[offset..offset + len].copy_from_slice(&blob[at..at + len]);
        at += len;
    }
    let path = root().join(SMOKE_FAT_IMAGE);
    fs::write(&path, image).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// What the guest wrote to the FAT stick, read back by oceans-fat and, if
/// installed, by fsck.fat (on the PATH, or in WSL below
/// `OCEANS_FAT_TOOLS_WSL`, as for the library's tests).
fn check_smoke_fat() -> Result {
    struct Image(Vec<u8>);
    impl oceans_fat::Disk for Image {
        fn read_at(
            &mut self,
            offset: u64,
            out: &mut [u8],
        ) -> std::result::Result<(), oceans_fat::IoError> {
            let start = usize::try_from(offset).map_err(|_| oceans_fat::IoError)?;
            let bytes = self
                .0
                .get(start..start + out.len())
                .ok_or(oceans_fat::IoError)?;
            out.copy_from_slice(bytes);
            Ok(())
        }
        fn size(&self) -> u64 {
            self.0.len() as u64
        }
    }
    let path = root().join(SMOKE_FAT_IMAGE);
    let image = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut fat =
        oceans_fat::Fat::open(Image(image.clone())).map_err(|e| format!("FAT stick: {e:?}"))?;
    let mut read = |path: &str| -> std::result::Result<Vec<u8>, String> {
        let mut node = oceans_fat::ROOT;
        for part in path.split('/') {
            node = fat
                .lookup(node, part)
                .map_err(|e| format!("FAT stick: /usb/{path}: {e:?}"))?;
            fat.retain(node).map_err(|e| format!("{e:?}"))?;
        }
        let mut bytes = vec![0u8; fat.size(node).map_err(|e| format!("{e:?}"))? as usize];
        fat.read_file(node, 0, &mut bytes)
            .map_err(|e| format!("{e:?}"))?;
        Ok(bytes)
    };
    if read("outer/moved-dir/renamed-on-fat.txt")? != b"written on FAT\n"
        || read("outer/moved-dir/inside.txt")? != b"nested\n"
        || read("new.txt").is_ok()
        || read("oceans-dir").is_ok()
    {
        return Err("the FAT stick does not hold what the shell wrote".into());
    }
    if read("big.bin")? != big_body() {
        return Err("the FAT stick's big.bin does not match what the host served".into());
    }
    if read("outer/big.bin")? != big_body()
        || read("tree-moved/a.txt")? != b"copied tree\n"
        || read("tree-moved/inner/a.txt")? != b"copied tree\n"
    {
        return Err("the FAT stick does not hold what was copied and moved to it".into());
    }
    if read("HELLO.TXT").is_ok() {
        return Err("HELLO.TXT is still on the FAT stick".into());
    }
    let report = fat
        .check()
        .map_err(|e| format!("the FAT stick fails its check: {e:?}"))?;
    if report.lost + report.orphans + report.overlong != 0 {
        return Err(format!("the FAT stick needs repair: {report:?}"));
    }
    // The volume starts 1 MiB in (MBR); fsck.fat wants the volume alone.
    let volume = root().join("build/smoke-fat-volume.img");
    fs::write(&volume, &image[1 << 20..])
        .map_err(|e| format!("cannot write {}: {e}", volume.display()))?;
    match fsck_fat(&volume) {
        Some((true, _)) => println!("the FAT stick passes fsck.fat"),
        Some((false, output)) => {
            return Err(format!(
                "fsck.fat finds problems on the FAT stick:\n{output}"
            ));
        }
        None => println!("fsck.fat not available: the FAT stick was checked by oceans-fat only"),
    }
    println!("the files written to the FAT stick are there");
    Ok(())
}

/// Runs `fsck.fat -n` on an image, if the tool can be found.
fn fsck_fat(image: &Path) -> Option<(bool, String)> {
    let output = match env::var("OCEANS_FAT_TOOLS_WSL") {
        Ok(tools) => {
            let text = image.to_string_lossy().replace('\\', "/");
            let wsl = match text.split_once(":/") {
                Some((drive, rest)) => format!("/mnt/{}/{rest}", drive.to_ascii_lowercase()),
                None => text,
            };
            Command::new("wsl")
                .args(["-e", &format!("{tools}/usr/sbin/fsck.fat"), "-n", &wsl])
                .output()
                .ok()?
        }
        Err(_) => Command::new("fsck.fat")
            .arg("-n")
            .arg(image)
            .output()
            .ok()?,
    };
    if output.status.code() == Some(127) {
        return None;
    }
    Some((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

/// `go` for this repository: its own build and module caches (under
/// `build/`), so no machine-wide Go setting can break the build.
fn go() -> Command {
    let mut cmd = Command::new(env::var_os("OCEANS_GO").unwrap_or_else(|| "go".into()));
    let cache = root().join("build").join("go");
    cmd.current_dir(root().join("go"))
        .env("GOCACHE", cache.join("cache"))
        .env("GOMODCACHE", cache.join("mod"))
        .env("GOPATH", cache.join("path"))
        .env("GOFLAGS", "-mod=mod")
        .env("GOTOOLCHAIN", "local");
    cmd
}

/// Go programs (ADR-0050), built for WebAssembly into `build/go`: `(file
/// name, module)` for each `(package, file name)` of `programs`.
fn build_go(programs: &[(&str, &str)]) -> Result<Vec<(String, Vec<u8>)>> {
    let out = root().join("build").join("go");
    fs::create_dir_all(&out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let mut modules = Vec::new();
    for &(package, name) in programs {
        let path = out.join(name);
        run_command(
            go().env("GOOS", "wasip1")
                .env("GOARCH", "wasm")
                .args(["build", "-trimpath", "-o"])
                .arg(&path)
                .arg(package),
        )
        .map_err(|e| {
            format!("{e} (Go 1.26 or later is needed: https://go.dev/dl, or set OCEANS_GO)")
        })?;
        let bytes = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        modules.push((name.to_string(), bytes));
    }
    Ok(modules)
}

/// `gofmt`, `go vet` (host and wasip1) and `go test` for `go/`.
fn check_go() -> Result {
    let listed = Command::new(
        env::var_os("OCEANS_GO")
            .map(|go| Path::new(&go).with_file_name("gofmt"))
            .unwrap_or_else(|| "gofmt".into()),
    )
    .current_dir(root().join("go"))
    .args(["-l", "."])
    .output()
    .map_err(|e| format!("cannot run gofmt: {e}"))?;
    let files = String::from_utf8_lossy(&listed.stdout);
    if !files.trim().is_empty() {
        return Err(format!("Go files need gofmt:\n{files}"));
    }
    run_command(go().args(["vet", "./..."]))?;
    run_command(
        go().env("GOOS", "wasip1")
            .env("GOARCH", "wasm")
            .args(["vet", "./..."]),
    )?;
    run_command(go().args(["test", "./..."]))
}

/// The system's trusted roots as a PEM bundle (ADR-0054): Mozilla's set,
/// the one the Rust TLS stack trusts (`oceans_tls::web_roots`, from
/// webpki-roots), as whole certificates (webpki-root-certs, the same
/// release), since Go's crypto/x509 takes certificates rather than trust
/// anchors. Refuses to build if the two crates ever disagree.
fn ca_roots_pem() -> Result<String> {
    let certificates = webpki_root_certs::TLS_SERVER_ROOT_CERTS;
    let anchors = oceans_tls::web_roots().roots;
    let missing = anchors
        .iter()
        .filter(|anchor| {
            let key = anchor.subject_public_key_info.as_ref();
            !certificates
                .iter()
                .any(|certificate| contains(certificate.as_ref(), key))
        })
        .count();
    if certificates.len() != anchors.len() || missing != 0 {
        return Err(format!(
            "webpki-root-certs ({} certificates) and webpki-roots ({} anchors, {missing} \
             without a certificate) disagree: update them together",
            certificates.len(),
            anchors.len()
        ));
    }
    let mut pem = String::new();
    for certificate in certificates {
        pem.push_str("-----BEGIN CERTIFICATE-----\n");
        for line in base64(certificate.as_ref()).as_bytes().chunks(64) {
            pem.push_str(&String::from_utf8_lossy(line));
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
    }
    Ok(pem)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Standard base64 with padding (RFC 4648 §4).
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Bun (`OCEANS_BUN`, default `bun`) in `ui/`.
fn bun() -> Command {
    let mut cmd = Command::new(env::var_os("OCEANS_BUN").unwrap_or_else(|| "bun".into()));
    cmd.current_dir(root().join(UI_DIR));
    cmd
}

/// Runs Bun, explaining a missing Bun: it is a build requirement.
fn run_bun(args: &[&str]) -> Result {
    run_command(bun().args(args)).map_err(|e| {
        format!("{e} (Bun 1.3 or later is needed to build the web experience: https://bun.sh, or set OCEANS_BUN)")
    })
}

/// Installs the pinned dependencies (bun.lock, never updated here).
fn bun_install() -> Result {
    run_bun(&["install", "--frozen-lockfile"])
}

/// Builds the web experience (ADR-0058) into `ui/build` and copies it to
/// where the bridge embeds it: every file but the Brotli copies (the
/// bridge serves gzip).
fn build_ui() -> Result {
    bun_install()?;
    run_bun(&["run", "build"])?;
    let web = root().join(BRIDGE_WEB);
    if web.exists() {
        for entry in
            fs::read_dir(&web).map_err(|e| format!("cannot read {}: {e}", web.display()))?
        {
            let path = entry
                .map_err(|e| format!("cannot read {}: {e}", web.display()))?
                .path();
            if path.file_name().is_some_and(|name| name == ".gitkeep") {
                continue;
            }
            let removed = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            removed.map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
        }
    }
    let (files, bytes) = copy_tree(&root().join(UI_DIR).join("build"), &web, &|path: &Path| {
        path.extension().is_none_or(|ext| ext != "br")
    })?;
    println!(
        "web experience built: {files} files, {} KiB, into {BRIDGE_WEB}",
        bytes / 1024
    );
    Ok(())
}

/// Copies the files under `from` that `keep` accepts into `to`; returns
/// how many and their size.
fn copy_tree(from: &Path, to: &Path, keep: &dyn Fn(&Path) -> bool) -> Result<(usize, u64)> {
    fs::create_dir_all(to).map_err(|e| format!("cannot create {}: {e}", to.display()))?;
    let (mut files, mut bytes) = (0, 0);
    for entry in fs::read_dir(from).map_err(|e| format!("cannot read {}: {e}", from.display()))? {
        let path = entry
            .map_err(|e| format!("cannot read {}: {e}", from.display()))?
            .path();
        let target = to.join(path.file_name().expect("a directory entry has a name"));
        if path.is_dir() {
            let (f, b) = copy_tree(&path, &target, keep)?;
            files += f;
            bytes += b;
        } else if keep(&path) {
            copy(&path, &target)?;
            files += 1;
            bytes += fs::metadata(&path).map_or(0, |m| m.len());
        }
    }
    Ok((files, bytes))
}

/// The web experience's checks: licenses (MIT, Apache-2.0 and BSD only),
/// unit tests (`bun test`) and types (`svelte-check`).
fn check_ui() -> Result {
    bun_install()?;
    run_bun(&["run", "licenses"])?;
    run_bun(&["run", "test"])?;
    run_bun(&["run", "check"])
}

fn dev_seed() -> Result<[u8; 32]> {
    let hex = DEV_SEED.trim();
    let mut seed = [0u8; 32];
    if hex.len() != 64 {
        return Err("tools/keys/oceans-dev.seed is not 32 bytes of hex".into());
    }
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
            .map_err(|_| "tools/keys/oceans-dev.seed is not hex".to_string())?;
    }
    Ok(seed)
}

/// The example packages in `build/packages`: Hello 1.0.0 and 2.0.0, two
/// the system must refuse (signed by an untrusted key, and changed after
/// signing), the Heartbeat service, and the Go app Greeter (`go_apps`, as
/// `build_go` made them) as an app and as a service.
fn build_packages(user: &Path, go_apps: &[(String, Vec<u8>)]) -> Result {
    let program = fs::read(user.join(HELLO_PROGRAM))
        .map_err(|e| format!("cannot read {HELLO_PROGRAM}: {e}"))?;
    let seed = dev_seed()?;
    let v2 = HELLO_MANIFEST.replace("version = 1.0.0", "version = 2.0.0");
    let sign = |manifest: &str, seed: &[u8; 32]| {
        oceans_package::build(
            &[("manifest", manifest.as_bytes()), ("hello", &program)],
            seed,
        )
        .map_err(|e| format!("cannot build a package: {e:?}"))
    };
    let v1 = sign(HELLO_MANIFEST, &seed)?;
    // Same files, one byte of the program changed, the original signature.
    let signature = oceans_archive::Archive::parse(&v1)
        .ok()
        .and_then(|archive| archive.find(oceans_package::SIGNATURE).map(<[u8]>::to_vec))
        .ok_or("the package has no signature")?;
    let mut changed = program.clone();
    if let Some(last) = changed.last_mut() {
        *last ^= 0xff;
    }
    let forged: [(&str, &[u8]); 3] = [
        ("manifest", HELLO_MANIFEST.as_bytes()),
        ("hello", &changed),
        (oceans_package::SIGNATURE, &signature),
    ];
    let mut tampered = vec![0u8; oceans_archive::archive_len(&forged)];
    oceans_archive::write(&forged, &mut tampered)
        .map_err(|e| format!("cannot build a package: {e:?}"))?;
    let heartbeat = fs::read(user.join(HEARTBEAT_PROGRAM))
        .map_err(|e| format!("cannot read {HEARTBEAT_PROGRAM}: {e}"))?;
    let heartbeat = oceans_package::build(
        &[
            ("manifest", HEARTBEAT_MANIFEST.as_bytes()),
            ("heartbeat", &heartbeat),
        ],
        &seed,
    )
    .map_err(|e| format!("cannot build a package: {e:?}"))?;
    let notes = fs::read(user.join(NOTES_PROGRAM))
        .map_err(|e| format!("cannot read {NOTES_PROGRAM}: {e}"))?;
    let notes = oceans_package::build(
        &[("manifest", NOTES_MANIFEST.as_bytes()), ("notes", &notes)],
        &seed,
    )
    .map_err(|e| format!("cannot build a package: {e:?}"))?;
    let greeter = go_apps
        .iter()
        .find(|(name, _)| name == GREETER_PROGRAM)
        .map(|(_, bytes)| bytes.as_slice())
        .ok_or("the greeter Go app was not built")?;
    // The same program as a service (ADR-0049): no console, so the Go
    // host sends its output to the log.
    let greeter_service = GREETER_MANIFEST
        .replace("id = app.oceans.greeter", "id = app.oceans.greeter-service")
        .replace("name = Greeter", "name = Greeter Service")
        .replace("runtime = wasm", "runtime = wasm\nkind = service");
    let greeter_package = |manifest: &str| {
        oceans_package::build(
            &[
                ("manifest", manifest.as_bytes()),
                (GREETER_PROGRAM, greeter),
            ],
            &seed,
        )
        .map_err(|e| format!("cannot build a package: {e:?}"))
    };
    let tiles = go_apps
        .iter()
        .find(|(name, _)| name == TILES_PROGRAM)
        .ok_or("the tiles Go app was not built")?;
    let tiles = oceans_package::build(
        &[
            ("manifest", TILES_MANIFEST.as_bytes()),
            (TILES_PROGRAM, &tiles.1),
        ],
        &seed,
    )
    .map_err(|e| format!("cannot build a package: {e:?}"))?;
    let store_index = store_index(&tiles);
    let dir = root().join(PACKAGES_DIR);
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    for (name, bytes) in [
        ("heartbeat-1.0.0.opk", heartbeat),
        ("notes-1.0.0.opk", notes),
        ("tiles-1.0.0.opk", tiles),
        ("index.json", store_index.into_bytes()),
        ("hello-1.0.0.opk", v1),
        ("hello-2.0.0.opk", sign(&v2, &seed)?),
        ("untrusted.opk", sign(HELLO_MANIFEST, &UNTRUSTED_SEED)?),
        ("tampered.opk", tampered),
        ("greeter-1.0.0.opk", greeter_package(GREETER_MANIFEST)?),
        (
            "greeter-service-1.0.0.opk",
            greeter_package(&greeter_service)?,
        ),
    ] {
        let path = dir.join(name);
        fs::write(&path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// A blank (zero-filled) image of `size` bytes; an existing one is
/// replaced only if `fresh`.
fn prepare_blank(path: &str, size: usize, fresh: bool) -> Result {
    let path = root().join(path);
    if path.is_file() && !fresh {
        return Ok(());
    }
    fs::write(&path, vec![0u8; size]).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Third-party apps for the smoke test (ADR-0062, the exit criterion of
/// Phase 8): made, built and signed outside this repository with the SDK
/// and its developer tool, as a developer would: a new key, a Rust app and
/// a Go app from the templates. Their packages are served as
/// `/packages/third-party-*.opk`, the key's public half as
/// `third-party.pub` (the script trusts it).
fn build_third_party() -> Result {
    run_command(cargo().args(["build", "--quiet", "--package", "oceans-dev"]))?;
    let tool = root()
        .join("target/debug/oceans")
        .with_extension(env::consts::EXE_EXTENSION);
    let dir = env::temp_dir().join("oceans-third-party");
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| format!("cannot clear {}: {e}", dir.display()))?;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let cache = root().join("build").join("go");
    let oceans = |args: &[&str]| {
        let mut cmd = Command::new(&tool);
        cmd.args(args)
            .current_dir(&dir)
            .env("OCEANS_SDK", root())
            .env("GOCACHE", cache.join("cache"))
            .env("GOMODCACHE", cache.join("mod"))
            .env("GOPATH", cache.join("path"))
            .env("GOTOOLCHAIN", "local");
        if let Some(go) = env::var_os("OCEANS_GO") {
            cmd.env("OCEANS_GO", go);
        }
        if let Some(bun) = env::var_os("OCEANS_BUN") {
            cmd.env("OCEANS_BUN", bun);
        }
        run_command(&mut cmd)
    };
    oceans(&["keygen", "Example", "Developer"])?;
    oceans(&["new", "rust", "app.example.counter"])?;
    oceans(&["new", "go", "app.example.hello", "--name", "Hello Go"])?;
    oceans(&["build", "counter", "--key", "oceans-developer.key"])?;
    oceans(&["build", "hello", "--key", "oceans-developer.key"])?;
    // A web app (ADR-0064): SvelteKit, built with Bun.
    oceans(&["new", "sveltekit", "app.example.notes"])?;
    oceans(&["build", "notes", "--key", "oceans-developer.key"])?;
    let packages = root().join(PACKAGES_DIR);
    for (from, to) in [
        (
            "counter/dist/app.example.counter-0.1.0.opk",
            "third-party-counter.opk",
        ),
        (
            "hello/dist/app.example.hello-0.1.0.opk",
            "third-party-hello.opk",
        ),
        (
            "notes/dist/app.example.notes-0.1.0.opk",
            "third-party-notes.opk",
        ),
    ] {
        copy(&dir.join(from), &packages.join(to))?;
    }
    let key = fs::read_to_string(dir.join("oceans-developer.key"))
        .map_err(|e| format!("the developer key: {e}"))?;
    let seed = key
        .lines()
        .find_map(|line| line.strip_prefix("seed = "))
        .ok_or("the developer key has no seed")?;
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(seed.get(2 * i..2 * i + 2).unwrap_or("zz"), 16)
            .map_err(|_| "the developer key's seed is not hex".to_string())?;
    }
    fs::write(
        packages.join("third-party.pub"),
        oceans_package::public_key_hex(&bytes),
    )
    .map_err(|e| format!("cannot write the developer's public key: {e}"))?;
    println!("third-party apps built with the SDK: a Rust app, a Go app and a SvelteKit web app");
    Ok(())
}

/// The Store's catalog (ADR-0061) the smoke test's HTTP server serves at
/// `/store/index.json`: Tiles, with the size and SHA-256 of its package.
fn store_index(tiles: &[u8]) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(tiles);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        r#"{{"apps":[{{"id":"app.oceans.tiles","name":"Tiles","version":"1.0.0","publisher":"Oceans Examples","description":"The example windowed Go app: a colour that changes with every key","permissions":["window"],"package":"tiles-1.0.0.opk","size":{},"sha256":"{hex}"}}]}}"#,
        tiles.len()
    )
}

/// A PPM (P6) screen capture: width, height, RGB bytes.
fn read_ppm(path: &Path) -> std::result::Result<(usize, usize, Vec<u8>), String> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(String::from_utf8_lossy(&bytes[start..at]).into_owned());
    }
    let number = |i: usize| {
        fields[i]
            .parse::<usize>()
            .map_err(|_| format!("{}: bad PPM header", path.display()))
    };
    if fields[0] != "P6" || number(3)? != 255 {
        return Err(format!("{}: not an 8-bit P6 image", path.display()));
    }
    let (width, height) = (number(1)?, number(2)?);
    let pixels = bytes[at + 1..].to_vec();
    if pixels.len() < width * height * 3 {
        return Err(format!("{}: truncated", path.display()));
    }
    Ok((width, height, pixels))
}

/// The desktop (ADR-0057) as QEMU showed it: the bar, the launcher and
/// the Terminal in their colours, then the permission dialog over the
/// dimmed desktop.
fn check_smoke_screens() -> Result {
    let pixel = |image: &(usize, usize, Vec<u8>), x: usize, y: usize| {
        let at = (y * image.0 + x) * 3;
        u32::from(image.2[at]) << 16 | u32::from(image.2[at + 1]) << 8 | u32::from(image.2[at + 2])
    };
    const SURFACE: u32 = 0x12_1a_2b;
    const TERMINAL: u32 = 0x07_0b_14;
    const DIALOG: u32 = 0x1b_26_3f;
    let desktop = read_ppm(&root().join("build/smoke-desktop.ppm"))?;
    let (w, h) = (desktop.0, desktop.1);
    for (what, x, y, want) in [
        ("the bar", 5, 5, SURFACE),
        ("the launcher", 20, h - 20, SURFACE),
        ("the Terminal", w - 20, h - 20, TERMINAL),
    ] {
        let got = pixel(&desktop, x, y);
        if got != want {
            return Err(format!(
                "desktop capture: {what} at {x},{y} is {got:06x}, not {want:06x}"
            ));
        }
    }
    let dialog = read_ppm(&root().join("build/smoke-dialog.ppm"))?;
    if pixel(&dialog, 308, 220) != DIALOG {
        return Err(format!(
            "dialog capture: no permission dialog at 308,220 ({:06x})",
            pixel(&dialog, 308, 220)
        ));
    }
    if pixel(&dialog, 5, 5) == SURFACE {
        return Err("dialog capture: the desktop behind the dialog is not dimmed".into());
    }
    // A window, and the keyboard focus (ADR-0059): Notes' frame is at the
    // top left of the Terminal's area, its paper inside; the focused title
    // bar has the focus colour, first Notes', then (Ctrl+Tab) the
    // Terminal's.
    const FOCUS_TITLE: u32 = 0x1f_3a_5f;
    const IDLE_TITLE: u32 = 0x18_23_3a;
    const PAPER: u32 = 0xf4_ef_e1;
    let notes_title = (700, 82);
    let notes_paper = (787, 334);
    let terminal_title = (w - 40, 60);
    for (file, checks) in [
        (
            "build/smoke-window.ppm",
            [
                ("Notes' title bar", notes_title, FOCUS_TITLE),
                ("Notes' paper", notes_paper, PAPER),
                ("the Terminal's title bar", terminal_title, IDLE_TITLE),
            ],
        ),
        (
            "build/smoke-focus.ppm",
            [
                ("Notes' title bar", notes_title, IDLE_TITLE),
                ("Notes' paper", notes_paper, PAPER),
                ("the Terminal's title bar", terminal_title, FOCUS_TITLE),
            ],
        ),
    ] {
        let image = read_ppm(&root().join(file))?;
        for (what, (x, y), want) in checks {
            let got = pixel(&image, x, y);
            if got != want {
                return Err(format!(
                    "{file}: {what} at {x},{y} is {got:06x}, not {want:06x}"
                ));
            }
        }
    }
    println!(
        "the desktop, its permission dialog, an app window and the keyboard focus are on the screen captures"
    );
    Ok(())
}

/// The files the guest stored on the NVMe disk, read from its image as an
/// Oceans volume (ADR-0040).
fn check_smoke_nvme() -> Result {
    use oceans_volume::{ROOT, Volume};

    let path = root().join(SMOKE_NVME_IMAGE);
    let image = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (mut volume, _) = Volume::open(ImageFile(image), false)
        .map_err(|e| format!("the NVMe disk does not mount on the host: {e:?}"))?;
    let mut read = |name: &str| -> std::result::Result<Vec<u8>, String> {
        let node = volume
            .lookup(ROOT, name)
            .map_err(|e| format!("/nvme/{name}: {e:?}"))?;
        let mut bytes = vec![0u8; volume.size(node).map_err(|e| format!("{e:?}"))? as usize];
        volume
            .read(node, 0, &mut bytes)
            .map_err(|e| format!("reading /nvme/{name}: {e:?}"))?;
        Ok(bytes)
    };
    if read("note.txt")? != format!("{NVME_TEXT}\n").as_bytes() {
        return Err("/nvme/note.txt does not hold what the shell wrote".into());
    }
    if read("big.bin")? != big_body() {
        return Err("/nvme/big.bin does not match the file copied there".into());
    }
    println!("the files written to /nvme are on the NVMe image");
    Ok(())
}

/// A blank USB stick image; an existing one is replaced only if `fresh`.
fn prepare_stick(path: &str, fresh: bool) -> Result {
    let path = root().join(path);
    if path.is_file() && !fresh {
        return Ok(());
    }
    fs::write(&path, vec![0u8; STICK_SIZE])
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The files the guest stored on the stick, read from its image as an
/// Oceans volume.
fn check_smoke_stick() -> Result {
    use oceans_volume::{ROOT, Volume};

    let path = root().join(SMOKE_STICK_IMAGE);
    let image = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (mut volume, _) = Volume::open(ImageFile(image), false)
        .map_err(|e| format!("the USB stick does not mount on the host: {e:?}"))?;
    let mut read = |name: &str| -> std::result::Result<Vec<u8>, String> {
        let node = volume
            .lookup(ROOT, name)
            .map_err(|e| format!("/usb/{name}: {e:?}"))?;
        let mut bytes = vec![0u8; volume.size(node).map_err(|e| format!("{e:?}"))? as usize];
        volume
            .read(node, 0, &mut bytes)
            .map_err(|e| format!("reading /usb/{name}: {e:?}"))?;
        Ok(bytes)
    };
    if read("note.txt")? != format!("{STICK_TEXT}\n").as_bytes() {
        return Err("/usb/note.txt does not hold what the shell wrote".into());
    }
    if read("big.bin")? != big_body() {
        return Err("/usb/big.bin does not match what the host served".into());
    }
    if read("moved.txt")? != b"moved\n" || read("copy.bin")? != big_body() {
        return Err("the file moved and the file copied to /usb are not on the stick".into());
    }
    if read("tree").is_ok() {
        return Err("/usb/tree is still on the stick after it was moved away".into());
    }
    println!("the files written to /usb are on the stick image");
    Ok(())
}

fn smoke(profile: Profile) -> Result {
    build_image(profile, Some("oceans.test=smoke"))?;
    build_third_party()?;
    prepare_disk(SMOKE_DISK_IMAGE, true)?;
    prepare_blank(SMOKE_NVME_IMAGE, NVME_SIZE, true)?;
    prepare_stick(SMOKE_STICK_IMAGE, true)?;
    prepare_fat_stick()?;
    println!("smoke boot 1 of 2: blank disk");
    smoke_boot(SHELL_SCRIPT, SHELL_EXPECT, Nic::Virtio)?;
    println!("smoke boot 2 of 2: the same disk, an Intel NIC instead of virtio-net");
    // A clean boot image: the first boot's firmware wrote into it (vvfat).
    build_image(profile, Some("oceans.test=smoke"))?;
    smoke_boot(REBOOT_SCRIPT, REBOOT_EXPECT, Nic::E1000e)?;
    check_smoke_disk()?;
    check_smoke_nvme()?;
    check_smoke_screens()?;
    check_smoke_stick()?;
    check_smoke_fat()?;
    println!("smoke test passed: kernel came online, files survived a reboot");
    Ok(())
}

/// What the guest's console showed.
enum Console {
    Line(String),
    /// The shell's prompt: it waits for the next command.
    Prompt,
}

const SHELL_PROMPT: &[u8] = b"oceans> ";
/// What `ui pair` prints before the code (ADR-0058).
const PAIRING_CODE: &str = "ui: pairing code: ";
const CONSENT_PROMPT: &[u8] = b"Allow? [y/N] ";

/// One headless boot: types `script` into the shell, one command per
/// prompt, and requires every `expected` line, the online banner and a
/// successful exit. The guest's network card is `nic`.
fn smoke_boot(script: &[&[u8]], expected: &[Expect], nic: Nic) -> Result {
    let udp_forward = free_udp_port()?;
    let tcp_forward = free_tcp_port()?;
    let udp_echo = udp_echo_probe(udp_forward);
    let tcp_echo = tcp_echo_probe(tcp_forward);
    let dns_port = dns_server(IPV4_LOOPBACK)?;
    let tcp_port = tcp_greeter(IPV4_LOOPBACK, "hello from the host")?;
    let http_port = http_server(IPV4_LOOPBACK)?;
    let https = HttpsServer::start()?;
    let models_port = https_model_server()?;
    // QEMU's user network connects the guest's IPv6 traffic for fec0::2
    // to the host's ::1 (ADR-0043).
    let dns6_port = dns_server(IPV6_LOOPBACK)?;
    let tcp6_port = tcp_greeter(IPV6_LOOPBACK, "hello from the host over IPv6")?;
    let http6_port = http_server(IPV6_LOOPBACK)?;
    // The third-party developer's public key (`build_third_party`).
    let developer_key =
        fs::read_to_string(root().join(PACKAGES_DIR).join("third-party.pub")).unwrap_or_default();
    let expand = |command: &[u8]| -> Vec<u8> {
        // Longer names first: `$HTTP` is a prefix of `$HTTPS` and `$HTTP6`.
        String::from_utf8_lossy(command)
            .replace("$MODELS", &models_port.to_string())
            .replace("$DNS6", &dns6_port.to_string())
            .replace("$DNS", &dns_port.to_string())
            .replace("$TCP6", &tcp6_port.to_string())
            .replace("$TCP", &tcp_port.to_string())
            .replace("$HTTPS", &https.port.to_string())
            .replace("$HTTP6", &http6_port.to_string())
            .replace("$HTTP", &http_port.to_string())
            .replace("$DEVKEY", &developer_key)
            .into_bytes()
    };
    let monitor_port = free_tcp_port()?;
    let bridge_port = free_tcp_port()?;
    let apps_port = free_tcp_port()?;
    // The pairing code `ui pair` printed (ADR-0058), for `@bridge`.
    let mut pairing_code: Option<String> = None;
    let mut child = qemu_command(
        true,
        nic,
        &Images {
            disk: SMOKE_DISK_IMAGE,
            nvme: SMOKE_NVME_IMAGE,
            stick: SMOKE_STICK_IMAGE,
            spare_stick: Some(SMOKE_FAT_IMAGE),
        },
        Some((udp_forward, tcp_forward)),
        Some((bridge_port, apps_port)),
        Some(monitor_port),
    )?
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()
    .map_err(|e| format!("failed to start QEMU: {e}"))?;
    let mut serial_input = child.stdin.take().expect("stdin is piped");

    let stdout = child.stdout.take().expect("stdout is piped");
    let (events_tx, events_rx) = mpsc::channel();
    // Used (and dropped) once typing may start, so the channel still
    // closes when QEMU exits.
    let mut prompt_again = Some(events_tx.clone());
    thread::spawn(move || {
        // Bytes, not lines: the prompt has no line ending.
        let mut line = Vec::new();
        for byte in BufReader::new(stdout)
            .bytes()
            .map_while(std::io::Result::ok)
        {
            if byte == b'\n' {
                let text = String::from_utf8_lossy(&line).into_owned();
                line.clear();
                if events_tx.send(Console::Line(text)).is_err() {
                    break;
                }
            } else {
                line.push(byte);
                // The consent question (ADR-0047) waits for an answer
                // like a prompt: the script's next line is that answer.
                let prompt = line.ends_with(SHELL_PROMPT) || line.ends_with(CONSENT_PROMPT);
                if prompt && events_tx.send(Console::Prompt).is_err() {
                    break;
                }
            }
        }
    });

    let deadline = Instant::now() + SMOKE_TIMEOUT;
    let mut online = false;
    let (mut shell_ready, mut prompt_waiting) = (false, false);
    let mut usb_settled = [false; USB_SETTLED.len()];
    let mut ready = false;
    let mut commands = script.iter().peekable();
    // Monitor commands waiting for a console line (`@when`).
    let mut pending: Option<(String, Vec<&[u8]>)> = None;
    let mut unmet: Vec<Expect> = expected.to_vec();
    let (mut udp_answered, mut tcp_answered) = (false, false);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match events_rx.recv_timeout(remaining) {
            Ok(Console::Line(line)) => {
                println!("  | {line}");
                online |= line.contains(ONLINE_BANNER);
                if let Some((_, code)) = line.split_once(PAIRING_CODE) {
                    pairing_code = Some(code.trim().to_string());
                }
                shell_ready |= line.contains(SHELL_READY);
                for (settled, marker) in usb_settled.iter_mut().zip(USB_SETTLED) {
                    *settled |= line.contains(marker);
                }
                unmet.retain(|expect| !expect.matches(&line));
                if pending
                    .as_ref()
                    .is_some_and(|(marker, _)| line.contains(marker.as_str()))
                    && let Some((_, queued)) = pending.take()
                {
                    for command in queued {
                        monitor_command(monitor_port, command)?;
                        thread::sleep(MONITOR_EVENT_GAP);
                    }
                }
                if !ready && shell_ready && usb_settled.iter().all(|&s| s) {
                    ready = true;
                    // The prompt came before the USB devices settled: act
                    // on it now.
                    if let Some(again) = prompt_again.take()
                        && prompt_waiting
                    {
                        let _ = again.send(Console::Prompt);
                    }
                }
            }
            // One command per prompt: typing while the guest (or QEMU, during
            // a disk flush) is busy overflows the 16-byte UART FIFO, because
            // QEMU's Windows stdio backend ignores backpressure.
            Ok(Console::Prompt) if ready => {
                if let Some(command) = commands.next() {
                    // The last command ends the boot: the host's probes into
                    // the guest's echo service must be done by then.
                    if commands.len() == 0 {
                        udp_answered = udp_answered || udp_echo.recv_timeout(PROBE_WAIT).is_ok();
                        tcp_answered = tcp_answered || tcp_echo.recv_timeout(PROBE_WAIT).is_ok();
                    }
                    // `@monitor COMMAND`: given to QEMU's monitor. It prints
                    // no prompt, so the next command follows once the guest
                    // has had time to react.
                    // `@bridge STEP`: the host checks the web experience
                    // (ADR-0058) over HTTP, then the next command follows.
                    let mut command = *command;
                    loop {
                        if let Some(line) = command.strip_prefix(b"@monitor ") {
                            monitor_command(monitor_port, line)?;
                            thread::sleep(MONITOR_SETTLE);
                        } else if let Some(probe) = command.strip_prefix(b"@screen ") {
                            if let Err(error) = expect_pixel(monitor_port, probe) {
                                let _ = child.kill();
                                let _ = child.wait();
                                return Err(error);
                            }
                        } else if let Some(text) = command.strip_prefix(b"@keys ") {
                            // `@keys TEXT`: pressed on the USB keyboard for
                            // a window (ADR-0059); no prompt follows.
                            press_usb_keys(monitor_port, text)?;
                            thread::sleep(MONITOR_SETTLE);
                        } else if let Some(step) = command.strip_prefix(b"@bridge ") {
                            if let Err(error) = check_bridge(
                                step,
                                (bridge_port, apps_port),
                                http_port,
                                pairing_code.as_deref(),
                            ) {
                                let _ = child.kill();
                                let _ = child.wait();
                                return Err(error);
                            }
                        } else {
                            break;
                        }
                        match commands.next() {
                            Some(next) => command = next,
                            None => break,
                        }
                    }
                    // `@usb TEXT`: pressed on the USB keyboard; its Enter
                    // brings the next prompt.
                    if let Some(text) = command.strip_prefix(b"@usb ") {
                        press_usb_keys(monitor_port, text)?;
                        continue;
                    }
                    if command.starts_with(b"@") {
                        continue;
                    }
                    for byte in expand(command) {
                        serial_input
                            .write_all(&[byte])
                            .and_then(|()| serial_input.flush())
                            .map_err(|e| format!("cannot type into the serial console: {e}"))?;
                        thread::sleep(TYPING_DELAY);
                    }
                    // `@when TEXT` after a command: the `@monitor` commands
                    // that follow go to QEMU's monitor once the console
                    // prints a line containing TEXT, while the command runs.
                    if let Some(marker) = commands
                        .peek()
                        .and_then(|next| next.strip_prefix(b"@when "))
                    {
                        let marker = String::from_utf8_lossy(marker).into_owned();
                        commands.next();
                        let mut queued = Vec::new();
                        while let Some(line) = commands
                            .peek()
                            .and_then(|next| next.strip_prefix(b"@monitor "))
                        {
                            queued.push(line);
                            commands.next();
                        }
                        pending = Some((marker, queued));
                    }
                }
            }
            Ok(Console::Prompt) => prompt_waiting = true,
            // Reader finished: QEMU closed stdout, i.e. exited.
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "smoke test timed out after {}s",
                    SMOKE_TIMEOUT.as_secs()
                ));
            }
        }
    }

    let status = child.wait().map_err(|e| format!("waiting for QEMU: {e}"))?;
    if !(udp_answered || udp_echo.try_recv().is_ok()) {
        return Err("the guest's UDP echo service never answered from the host".into());
    }
    if !(tcp_answered || tcp_echo.try_recv().is_ok()) {
        return Err("the guest's TCP echo service never answered from the host".into());
    }
    println!("UDP and TCP echo answered from the host through the guest's network stack");
    if !unmet.is_empty() {
        return Err(format!("shell output missing: {unmet:?}"));
    }
    match (online, status.code()) {
        (true, Some(QEMU_EXIT_SUCCESS)) => Ok(()),
        (false, _) => Err(format!(
            "kernel never printed `{ONLINE_BANNER}` (QEMU {status})"
        )),
        (true, _) => Err(format!("kernel came online but QEMU exited with {status}")),
    }
}

/// A disk image file as a block device, for mounting on the host.
struct ImageFile(Vec<u8>);

impl oceans_volume::BlockDevice for ImageFile {
    fn block_count(&self) -> u64 {
        (self.0.len() / oceans_volume::BLOCK_SIZE) as u64
    }

    fn read_block(
        &mut self,
        block: u64,
        out: &mut oceans_volume::BlockBuf,
    ) -> std::result::Result<(), oceans_volume::IoError> {
        let at = block as usize * oceans_volume::BLOCK_SIZE;
        let bytes = self.0.get(at..at + oceans_volume::BLOCK_SIZE);
        out.copy_from_slice(bytes.ok_or(oceans_volume::IoError)?);
        Ok(())
    }

    fn write_block(
        &mut self,
        _: u64,
        _: &oceans_volume::BlockBuf,
    ) -> std::result::Result<(), oceans_volume::IoError> {
        Err(oceans_volume::IoError) // read-only inspection
    }

    fn flush(&mut self) -> std::result::Result<(), oceans_volume::IoError> {
        Ok(())
    }
}

/// Mounts the smoke disk on the host with the same volume code: the file
/// the guest kept must be there with its text, and the removed one gone.
fn check_smoke_disk() -> Result {
    use oceans_volume::{ROOT, Volume};

    let path = root().join(SMOKE_DISK_IMAGE);
    let image = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (mut volume, _) = Volume::open(ImageFile(image), false)
        .map_err(|e| format!("{} does not mount on the host: {e:?}", path.display()))?;
    let lookup = |volume: &Volume<ImageFile>, dir, name| {
        volume
            .lookup(dir, name)
            .map_err(|e| format!("/{}: {e:?}", KEPT_PATH.join("/")))
    };
    let keep = lookup(&volume, ROOT, KEPT_PATH[0])?;
    let note = lookup(&volume, keep, KEPT_PATH[1])?;
    let mut text = vec![0u8; volume.size(note).map_err(|e| format!("{e:?}"))? as usize];
    volume
        .read(note, 0, &mut text)
        .map_err(|e| format!("reading the kept file: {e:?}"))?;
    // The shell's `write` ends the text with a newline.
    let big = lookup(&volume, keep, "big.bin")?;
    let mut downloaded = vec![0u8; volume.size(big).map_err(|e| format!("{e:?}"))? as usize];
    volume
        .read(big, 0, &mut downloaded)
        .map_err(|e| format!("reading the downloaded file: {e:?}"))?;
    if downloaded != big_body() {
        return Err("the file fetched over HTTP does not match what the host served".into());
    }
    // Fetched over the Intel NIC on the second boot (ADR-0041).
    let intel = lookup(&volume, keep, "e1000e.bin")?;
    let mut downloaded = vec![0u8; volume.size(intel).map_err(|e| format!("{e:?}"))? as usize];
    volume
        .read(intel, 0, &mut downloaded)
        .map_err(|e| format!("reading the file fetched over e1000e: {e:?}"))?;
    if downloaded != big_body() {
        return Err("the file fetched over e1000e does not match what the host served".into());
    }
    let secure = lookup(&volume, keep, "tls.bin")?;
    let mut downloaded = vec![0u8; volume.size(secure).map_err(|e| format!("{e:?}"))? as usize];
    volume
        .read(secure, 0, &mut downloaded)
        .map_err(|e| format!("reading the file fetched over HTTPS: {e:?}"))?;
    if downloaded != tls_body() {
        return Err("the file fetched over HTTPS does not match what the host served".into());
    }
    if text != format!("{KEPT_TEXT}\n").as_bytes() {
        return Err(format!(
            "kept file holds {:?}",
            String::from_utf8_lossy(&text)
        ));
    }
    if volume.lookup(keep, "gone.txt").is_ok() {
        return Err("a removed file is still on the disk".into());
    }
    // Apps (ADR-0045): removed by the second boot, with every step of its
    // life in the audit log.
    if let Ok(apps) = volume.lookup(ROOT, "apps")
        && volume.lookup(apps, "app.oceans.hello").is_ok()
    {
        return Err("a removed app is still in /apps".into());
    }
    let system = lookup(&volume, ROOT, "system")?;
    let audit = lookup(&volume, system, "audit.log")?;
    let mut log = vec![0u8; volume.size(audit).map_err(|e| format!("{e:?}"))? as usize];
    volume
        .read(audit, 0, &mut log)
        .map_err(|e| format!("reading the audit log: {e:?}"))?;
    let log = String::from_utf8_lossy(&log);
    for event in [
        "installed app.oceans.hello 1.0.0 from Oceans Examples",
        "denied app.oceans.hello network (at its prompt)",
        "allowed app.oceans.hello network (by command)",
        "updated app.oceans.hello 1.0.0 -> 2.0.0",
        "stopped app.oceans.hello: a permission it used was revoked",
        "rolled back app.oceans.hello 2.0.0 -> 1.0.0",
        "removed app.oceans.hello",
        "enabled service app.oceans.heartbeat (starts at boot)",
        "disabled service app.oceans.heartbeat",
    ] {
        if !log.contains(event) {
            return Err(format!("the audit log lacks `{event}`"));
        }
    }
    // The AI service's settings, in the directory init granted it
    // (ADR-0053).
    let ai_dir = lookup(&volume, system, "ai")?;
    let settings = lookup(&volume, ai_dir, "model.conf")?;
    let mut text = vec![0u8; volume.size(settings).map_err(|e| format!("{e:?}"))? as usize];
    volume
        .read(settings, 0, &mut text)
        .map_err(|e| format!("reading the AI settings: {e:?}"))?;
    if !String::from_utf8_lossy(&text).contains(" oceans-test") {
        return Err("the AI settings were not stored in /system/ai".into());
    }
    // Copied from FAT (ADR-0039); moved away, or removed with `rm -r`.
    let mut node = keep;
    for name in ["docs-copy", "notes", "deep.txt"] {
        node = volume
            .lookup(node, name)
            .map_err(|e| format!("/keep/docs-copy/notes/deep.txt: {e:?}"))?;
    }
    let mut copied = vec![0u8; volume.size(node).map_err(|e| format!("{e:?}"))? as usize];
    volume
        .read(node, 0, &mut copied)
        .map_err(|e| format!("reading the file copied from FAT: {e:?}"))?;
    if copied != b"deep\n" {
        return Err("the file copied from the FAT stick does not match".into());
    }
    if ["tree", "tree-back"]
        .iter()
        .any(|name| volume.lookup(keep, name).is_ok())
    {
        return Err("a tree removed or moved away is still on the disk".into());
    }
    println!(
        "disk verified on the host: generation {}, /{} intact",
        volume.generation(),
        KEPT_PATH.join("/")
    );
    Ok(())
}

/// A UDP port free on the host now, for QEMU to forward.
fn free_udp_port() -> Result<u16> {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .map(|address| address.port())
        .map_err(|e| format!("cannot find a free UDP port: {e}"))
}

/// Sends datagrams from the host to the guest's UDP echo service (through
/// QEMU's port forwarding) until one comes back; reports success once.
fn udp_echo_probe(port: u16) -> mpsc::Receiver<()> {
    const PROBE: &[u8] = b"oceans udp echo probe";
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let Ok(socket) = UdpSocket::bind("127.0.0.1:0") else {
            return;
        };
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
        let deadline = Instant::now() + SMOKE_TIMEOUT;
        let mut reply = [0u8; 64];
        while Instant::now() < deadline {
            if socket.send_to(PROBE, ("127.0.0.1", port)).is_err() {
                thread::sleep(Duration::from_millis(500));
                continue;
            }
            if let Ok((len, _)) = socket.recv_from(&mut reply)
                && &reply[..len] == PROBE
            {
                let _ = done_tx.send(());
                return;
            }
        }
    });
    done_rx
}

/// A TCP port free on the host now, for QEMU to forward.
fn free_tcp_port() -> Result<u16> {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|e| format!("cannot find a free TCP port: {e}"))
}

/// Connects from the host to the guest's TCP echo service (through QEMU's
/// port forwarding) until a probe comes back; reports success once.
fn tcp_echo_probe(port: u16) -> mpsc::Receiver<()> {
    const PROBE: &[u8] = b"oceans tcp echo probe";
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let deadline = Instant::now() + SMOKE_TIMEOUT;
        while Instant::now() < deadline {
            let attempt = (|| -> std::io::Result<bool> {
                let mut stream = TcpStream::connect(("127.0.0.1", port))?;
                stream.set_read_timeout(Some(Duration::from_secs(2)))?;
                stream.write_all(PROBE)?;
                let mut reply = vec![0u8; PROBE.len()];
                let mut got = 0;
                while got < reply.len() {
                    match stream.read(&mut reply[got..])? {
                        0 => return Ok(false),
                        n => got += n,
                    }
                }
                Ok(reply == PROBE)
            })();
            if let Ok(true) = attempt {
                let _ = done_tx.send(());
                return;
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
    done_rx
}

/// Where the host's test servers listen: loopback, which QEMU's user
/// network reaches as 10.0.2.2 and (IPv6) fec0::2.
const IPV4_LOOPBACK: &str = "127.0.0.1:0";
const IPV6_LOOPBACK: &str = "[::1]:0";

/// A name the host's DNS server knows: (name, A, AAAA).
type DnsName = (&'static str, Option<[u8; 4]>, Option<[u8; 16]>);

const DNS_NAMES: &[DnsName] = &[
    (
        "oceans.test",
        Some([10, 1, 2, 3]),
        Some([0x20, 1, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 1, 0, 2, 0, 3]),
    ),
    (
        "ipv6.oceans.test",
        None,
        Some([0x20, 1, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 6]),
    ),
    // The https model server (ADR-0054), on the host.
    ("models.oceans.test", Some([10, 0, 2, 2]), None),
];

/// The question name of a DNS query, as text (no compression in
/// questions; `None` if malformed).
fn query_name(query: &[u8]) -> Option<String> {
    let mut at = 12;
    let mut labels = Vec::new();
    loop {
        let len = usize::from(*query.get(at)?);
        if len == 0 {
            return Some(labels.join("."));
        }
        let label = query.get(at + 1..at + 1 + len)?;
        labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
        at += 1 + len;
    }
}

/// A DNS server on the host for the guest's resolver, answering for
/// [`DNS_NAMES`] (every other name does not exist), listening at `bind`.
/// Returns its UDP port.
fn dns_server(bind: &str) -> Result<u16> {
    let socket =
        UdpSocket::bind(bind).map_err(|e| format!("cannot start the DNS server at {bind}: {e}"))?;
    let port = socket.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
        let deadline = Instant::now() + 2 * SMOKE_TIMEOUT;
        let mut query = [0u8; 512];
        let mut answer = [0u8; 512];
        while Instant::now() < deadline {
            let Ok((len, from)) = socket.recv_from(&mut query) else {
                continue;
            };
            let query = &query[..len];
            let name = query_name(query);
            let (a, aaaa) = DNS_NAMES
                .iter()
                .find(|(known, _, _)| name.as_deref() == Some(*known))
                .map_or((None, None), |&(_, a, aaaa)| (a, aaaa));
            if let Ok(len) = oceans_dns::respond_with(query, a, aaaa, &mut answer) {
                let _ = socket.send_to(&answer[..len], from);
            }
        }
    });
    Ok(port)
}

/// A TCP server on the host, listening at `bind`: answers each line with
/// `greeting`, then closes. Returns its port.
fn tcp_greeter(bind: &str, greeting: &'static str) -> Result<u16> {
    let listener = TcpListener::bind(bind)
        .map_err(|e| format!("cannot start the TCP server at {bind}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut line = Vec::new();
            let mut byte = [0u8; 1];
            while let Ok(1) = stream.read(&mut byte) {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            let reply = format!("{greeting}: {}\n", String::from_utf8_lossy(&line));
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    Ok(port)
}

/// The smoke test's model server (ADR-0051): an OpenAI-compatible Chat
/// Completions endpoint that follows a script, so the agent loop, its
/// tools, approvals and refusals are tested without a real model. What it
/// answers depends on the user's question and on how many tool results
/// the conversation already holds; it quotes the last tool result back.
fn fake_model(request: &str) -> String {
    let user = json_string_after(request, "\"role\":\"user\",\"content\":\"").unwrap_or_default();
    let done = request.matches("\"role\":\"tool\"").count();
    let last = request
        .rfind("\"role\":\"tool\",\"content\":\"")
        .and_then(|at| json_string_after(&request[at..], "\"content\":\""))
        .unwrap_or_default();
    let say = |content: &str| {
        format!(
            "{{\"choices\":[{{\"index\":0,\"message\":{{\"role\":\"assistant\",\"content\":\"{content}\"}},\"finish_reason\":\"stop\"}}]}}"
        )
    };
    let call = |name: &str, arguments: &str| {
        let arguments = arguments.replace('"', "\\\"");
        format!(
            "{{\"choices\":[{{\"index\":0,\"message\":{{\"role\":\"assistant\",\"content\":\"\",\"tool_calls\":[{{\"id\":\"call_{done}\",\"type\":\"function\",\"function\":{{\"name\":\"{name}\",\"arguments\":\"{arguments}\"}}}}]}},\"finish_reason\":\"tool_calls\"}}]}}"
        )
    };
    if user.contains("notes") {
        match done {
            0 => call("files_read", r#"{"path":"notes.txt"}"#),
            _ if last.contains("denied") => say("Understood: I did not read your notes."),
            _ => say(&format!("Your notes say: {last}")),
        }
    } else if user.contains("memory") {
        match done {
            0 => call("system_memory", "{}"),
            _ => say(&format!("Memory: {last}")),
        }
    } else if user.contains("start") {
        match done {
            0 => call("apps_list", "{}"),
            1 => call("apps_start", r#"{"id":"app.oceans.hello","args":"wait"}"#),
            _ => say(&format!("Done: {last}")),
        }
    } else if user.contains("stop") {
        match done {
            0 => call("apps_stop", r#"{"id":"app.oceans.hello"}"#),
            _ if last.contains("denied") => say("Understood: I left Hello running."),
            _ => say(&format!("Done: {last}")),
        }
    } else {
        say("I can look at memory and processes, and start or stop apps.")
    }
}

/// The JSON string (still escaped) that follows `marker` in `text`.
fn json_string_after(text: &str, marker: &str) -> Option<String> {
    let start = text.find(marker)? + marker.len();
    let mut out = String::new();
    let mut escaped = false;
    for c in text[start..].chars() {
        match c {
            '"' if !escaped => return Some(out),
            '\\' if !escaped => escaped = true,
            _ => escaped = false,
        }
        out.push(c);
    }
    None
}

/// The body of `/big`: 1 MiB in a pattern that catches reordering.
fn big_body() -> Vec<u8> {
    (0..1_048_576u32).map(|i| (i % 251) as u8).collect()
}

/// How long the guest gets to notice a monitor command (a USB device
/// plugged in or out).
const MONITOR_SETTLE: Duration = Duration::from_secs(2);
/// Between input events given to the monitor (`@when`): long enough for
/// the guest to poll each into a report of its own.
const MONITOR_EVENT_GAP: Duration = Duration::from_millis(300);

/// Presses `text` (lowercase letters, digits, space, CR) on the guest's
/// USB keyboard through QEMU's monitor (`sendkey`).
fn press_usb_keys(port: u16, text: &[u8]) -> Result {
    for &byte in text {
        let key = match byte {
            b'a'..=b'z' | b'0'..=b'9' => (byte as char).to_string(),
            b' ' => "spc".to_string(),
            b'\r' | b'\n' => "ret".to_string(),
            other => return Err(format!("no USB key for {:?}", other as char)),
        };
        monitor_command(port, format!("sendkey {key}").as_bytes())?;
        // One key at a time: sendkey holds each for 100 ms.
        thread::sleep(Duration::from_millis(150));
    }
    Ok(())
}

/// An `Authorization` header with `token`.
fn bearer_for(token: &str) -> String {
    format!("Authorization: Bearer {token}")
}

/// `@screen X Y RRGGBB WHAT`: captures the screen until pixel `X`, `Y` has
/// colour `RRGGBB` (an app may take a while to draw), or fails after a
/// minute.
fn expect_pixel(port: u16, probe: &[u8]) -> Result {
    let probe = String::from_utf8_lossy(probe);
    let mut words = probe.splitn(4, ' ');
    let (Some(x), Some(y), Some(rgb), Some(what)) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return Err(format!("bad @screen step: {probe}"));
    };
    let (Ok(x), Ok(y), Ok(want)) = (
        x.parse::<usize>(),
        y.parse::<usize>(),
        u32::from_str_radix(rgb, 16),
    ) else {
        return Err(format!("bad @screen step: {probe}"));
    };
    let path = root().join("build/smoke-probe.ppm");
    let until = Instant::now() + Duration::from_secs(60);
    let mut got = None;
    while Instant::now() < until {
        let _ = fs::remove_file(&path);
        monitor_command(port, b"screendump build/smoke-probe.ppm")?;
        thread::sleep(Duration::from_millis(400));
        if let Ok(image) = read_ppm(&path)
            && x < image.0
            && y < image.1
        {
            let at = (y * image.0 + x) * 3;
            let pixel = u32::from(image.2[at]) << 16
                | u32::from(image.2[at + 1]) << 8
                | u32::from(image.2[at + 2]);
            if pixel == want {
                // Kept for a look: build/smoke-screen-<what>.ppm.
                let slug: String = what
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() {
                            c.to_ascii_lowercase()
                        } else {
                            '-'
                        }
                    })
                    .collect();
                let _ = fs::copy(&path, root().join(format!("build/smoke-screen-{slug}.ppm")));
                println!("screen: {what} at {x},{y}");
                return Ok(());
            }
            got = Some(pixel);
        }
    }
    Err(format!(
        "screen: no {what}: {x},{y} is {}, not {want:06x}",
        got.map_or("unreadable".to_string(), |p| format!("{p:06x}"))
    ))
}

/// Gives QEMU's (human) monitor one command line.
fn monitor_command(port: u16, line: &[u8]) -> Result {
    let mut monitor = TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("cannot reach QEMU's monitor: {e}"))?;
    monitor
        .write_all(line)
        .and_then(|()| monitor.write_all(b"\n"))
        .map_err(|e| format!("cannot talk to QEMU's monitor: {e}"))?;
    // Let QEMU read the line before the connection closes.
    thread::sleep(Duration::from_millis(100));
    Ok(())
}

/// The TLS test material (ADR-0031): a CA, and a certificate it issued for
/// 10.0.2.2 with its key. Test-only; the keys are public.
const TLS_TEST_CA: &[u8] = include_bytes!("../../../libs/tls/testdata/ecdsa-ca.pem");
const TLS_TEST_CHAIN: &str = "libs/tls/testdata/server-ecdsa.pem";
const TLS_TEST_KEY: &str = "libs/tls/testdata/server.key.pem";
/// Served at `/tls.txt` and `/tls.bin` over HTTPS.
const TLS_TEXT: &[u8] = b"hello over https\n";

/// The body of `/tls.bin`: 256 KiB of numbered lines. Printable text only:
/// OpenSSL on Windows reads served files in text mode, where 0x1A ends a
/// file.
fn tls_body() -> Vec<u8> {
    (0..4096u32)
        .flat_map(|i| format!("line {i:05} {:>52}\n", i % 977).into_bytes())
        .collect()
}

/// OpenSSL's `s_server` serving files over HTTPS for the guest's `fetch`:
/// an independent TLS implementation. Stopped when dropped.
struct HttpsServer {
    port: u16,
    child: std::process::Child,
}

impl HttpsServer {
    fn start() -> Result<Self> {
        let www = root().join("build/https");
        fs::create_dir_all(&www).map_err(|e| format!("cannot create {}: {e}", www.display()))?;
        fs::write(www.join("tls.txt"), TLS_TEXT)
            .and_then(|()| fs::write(www.join("tls.bin"), tls_body()))
            .map_err(|e| format!("cannot write the HTTPS files: {e}"))?;
        let port = free_tcp_port()?;
        let child = Command::new("openssl")
            .args(["s_server", "-quiet", "-WWW", "-accept"])
            .arg(format!("127.0.0.1:{port}"))
            .arg("-cert")
            .arg(root().join(TLS_TEST_CHAIN))
            .arg("-key")
            .arg(root().join(TLS_TEST_KEY))
            .current_dir(&www)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start `openssl s_server` (is OpenSSL installed?): {e}"))?;
        let server = Self { port, child };
        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            if Instant::now() > deadline {
                return Err("`openssl s_server` did not start listening".into());
            }
            thread::sleep(Duration::from_millis(50));
        }
        Ok(server)
    }
}

impl Drop for HttpsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An HTTP server on the host for the guest's `fetch`, listening at
/// `bind`. Returns its port.
fn http_server(bind: &str) -> Result<u16> {
    let listener = TcpListener::bind(bind)
        .map_err(|e| format!("cannot start the HTTP server at {bind}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            serve_http(&mut stream, false);
        }
    });
    Ok(port)
}

/// The test model server's certificate for models.oceans.test, its key,
/// and the test CA that issued it (tools/xtask/testdata, ADR-0054).
/// Test-only; the keys are public.
const MODELS_CA: &[u8] = include_bytes!("../testdata/models-ca.pem");
const MODELS_CERT: &[u8] = include_bytes!("../testdata/models.der");
const MODELS_KEY: &[u8] = include_bytes!("../testdata/models.key.der");

/// Randomness for the test https server: the in-tree generator, seeded
/// from the clock and the process. Its keys are public test keys; this
/// only has to make handshakes differ.
#[derive(Debug)]
struct ServerRandom;

static SERVER_RNG: std::sync::Mutex<oceans_random::Rng> =
    std::sync::Mutex::new(oceans_random::Rng::new());

impl rustls::crypto::SecureRandom for ServerRandom {
    fn fill(&self, out: &mut [u8]) -> std::result::Result<(), rustls::crypto::GetRandomFailed> {
        let mut rng = SERVER_RNG
            .lock()
            .map_err(|_| rustls::crypto::GetRandomFailed)?;
        if !rng.is_seeded() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            let mut seed = now.as_nanos().to_le_bytes().to_vec();
            seed.extend_from_slice(&std::process::id().to_le_bytes());
            rng.reseed(&seed);
        }
        rng.fill(out);
        Ok(())
    }
}

/// The smoke test's model server over https (ADR-0054): the HTTP server's
/// answers (the scripted model at `/v1/chat/completions`) behind TLS from
/// the in-tree TLS stack's server side, with the certificate for
/// models.oceans.test. On the host's loopback (the guest's 10.0.2.2);
/// returns its port. (`openssl s_server -WWW` only serves files: the
/// model needs POST.)
fn https_model_server() -> Result<u16> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::sync::Arc;

    let provider = Arc::new(oceans_tls::provider(&ServerRandom));
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(|builder| {
            builder.with_no_client_auth().with_single_cert(
                vec![CertificateDer::from(MODELS_CERT.to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(MODELS_KEY.to_vec())),
            )
        })
        .map_err(|e| format!("cannot set up the https model server: {e}"))?;
    let config = Arc::new(config);
    let listener = TcpListener::bind(IPV4_LOOPBACK)
        .map_err(|e| format!("cannot start the https model server: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            let config = Arc::clone(&config);
            // A connection each: the guest's handshake is interpreted Go.
            thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
                let Ok(connection) = rustls::ServerConnection::new(config) else {
                    return;
                };
                let mut tls = rustls::StreamOwned::new(connection, stream);
                serve_http(&mut tls, true);
                tls.conn.send_close_notify();
                let _ = tls.flush();
            });
        }
    });
    Ok(port)
}

/// Answers one HTTP request on `stream` (the test servers' content).
fn serve_http(stream: &mut (impl Read + Write), over_tls: bool) {
    let mut request = Vec::new();
    let mut byte = [0u8; 1];
    while !request.ends_with(b"\r\n\r\n") && request.len() < 8192 {
        match stream.read(&mut byte) {
            Ok(1) => request.push(byte[0]),
            _ => break,
        }
    }
    let request = String::from_utf8_lossy(&request).into_owned();
    // A POST's body (the fake model server reads it).
    let length = request
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0)
        .min(1 << 20);
    let mut body = vec![0u8; length];
    if stream.read_exact(&mut body).is_err() {
        body.clear();
    }
    let body = String::from_utf8_lossy(&body).into_owned();
    let path = request.split(' ').nth(1).unwrap_or("").to_string();
    let fixed = |status: &str, body: &[u8]| {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    };
    let response = match path.as_str() {
        "/hello.txt" => fixed("200 OK", b"hello over http\n"),
        "/ipv6.txt" => fixed("200 OK", b"hello over http on ipv6\n"),
        "/moved.txt" => fixed("200 OK", b"you were redirected\n"),
        "/redirect" => b"HTTP/1.1 302 Found\r\nLocation: /moved.txt\r\nContent-Length: 0\r\n\r\n".to_vec(),
        "/chunked" => b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\nchunked \r\nF\r\ntransfer works\n\r\n0\r\n\r\n".to_vec(),
        "/big" => fixed("200 OK", &big_body()),
        "/ca.pem" => fixed("200 OK", TLS_TEST_CA),
        "/models-ca.pem" => fixed("200 OK", MODELS_CA),
        "/v1/chat/completions" => {
            let mut reply = fake_model(&body);
            // Over https the model says so: the smoke test can tell the
            // answers apart.
            if over_tls {
                reply = reply.replace("Memory: ", "Memory over https: ");
            }
            fixed("200 OK", reply.as_bytes())
        }
        // The example packages (ADR-0046), by plain file name.
        package
            if package
                .strip_prefix("/packages/")
                .or_else(|| package.strip_prefix("/store/"))
                .is_some_and(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
                    && !name.contains("..")
            }) =>
        {
            let name = package
                .strip_prefix("/packages/")
                .or_else(|| package.strip_prefix("/store/"))
                .unwrap_or_default();
            match fs::read(root().join(PACKAGES_DIR).join(name)) {
                Ok(bytes) => fixed("200 OK", &bytes),
                Err(_) => fixed("404 Not Found", b"no such package\n"),
            }
        }
        _ => fixed("404 Not Found", b"not here\n"),
    };
    let _ = stream.write_all(&response);
}

/// A response from the bridge, as the smoke test reads it.
struct HttpReply {
    status: u16,
    /// Header lines, lower-cased names.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpReply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// How long one request to the bridge may take (it runs interpreted Go).
const BRIDGE_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// One HTTP/1.1 request to the guest's bridge through QEMU's forwarding:
/// `headers` are extra lines (`Name: value`); a body is sent as JSON.
fn bridge_request(
    port: u16,
    method: &str,
    path: &str,
    headers: &[&str],
    body: Option<&str>,
) -> Result<HttpReply> {
    let started = Instant::now();
    let fail = |e: std::io::Error| format!("{method} {path} to the bridge: {e}");
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(fail)?;
    stream
        .set_read_timeout(Some(BRIDGE_REQUEST_TIMEOUT))
        .map_err(fail)?;
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    for header in headers {
        request.push_str(header);
        request.push_str("\r\n");
    }
    if let Some(body) = body {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ));
    } else {
        request.push_str("\r\n");
    }
    stream.write_all(request.as_bytes()).map_err(fail)?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(fail)?;
    let end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| format!("{method} {path}: no complete response from the bridge"))?;
    let head = String::from_utf8_lossy(&raw[..end]).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("{method} {path}: malformed status line from the bridge"))?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let reply = HttpReply {
        status,
        headers,
        body: raw[end + 4..].to_vec(),
    };
    println!(
        "  bridge: {method} {path} -> {status}, {} bytes in {} ms",
        reply.body.len(),
        started.elapsed().as_millis()
    );
    Ok(reply)
}

/// Requires `status` and each of `contains` in the body.
fn expect_reply(reply: &HttpReply, what: &str, status: u16, contains: &[&str]) -> Result {
    let text = reply.text();
    if reply.status != status {
        return Err(format!(
            "bridge: {what}: status {} (wanted {status}): {text}",
            reply.status
        ));
    }
    if let Some(missing) = contains.iter().find(|part| !text.contains(*part)) {
        return Err(format!("bridge: {what}: no `{missing}` in {text}"));
    }
    Ok(())
}

/// The host's checks of the web experience (ADR-0058), `@bridge STEP`:
/// `paired` after `ui pair` (the page, refusals without the code, the
/// System API with it, apps started and stopped, an AI action approved
/// from the browser), `unpaired` after `ui unpair` (the code is dead).
fn check_bridge(
    step: &[u8],
    (port, apps_port): (u16, u16),
    http_port: u16,
    code: Option<&str>,
) -> Result {
    let code = code.ok_or("bridge: `ui pair` printed no pairing code")?;
    let bearer = format!("Authorization: Bearer {code}");
    let auth = [bearer.as_str()];
    let get = |path: &str, headers: &[&str]| bridge_request(port, "GET", path, headers, None);
    let post = |path: &str, headers: &[&str], body: &str| {
        bridge_request(port, "POST", path, headers, Some(body))
    };
    match step {
        b"paired" => {
            // The page, with its policy; the app's routes get it too.
            let page = get("/", &[])?;
            expect_reply(&page, "the page", 200, &["<title>Oceans</title>"])?;
            let policy = page.header("content-security-policy").unwrap_or("");
            if !policy.starts_with("default-src 'self'")
                || !policy.contains("frame-ancestors 'none'")
            {
                return Err(format!("bridge: the page's policy is {policy:?}"));
            }
            expect_reply(
                &get("/apps", &[])?,
                "a route",
                200,
                &["<title>Oceans</title>"],
            )?;
            // The app's script, compressed as a browser asks for it.
            let html = page.text();
            let script = html
                .split('"')
                .find(|part| part.starts_with("/_app/immutable/") && part.ends_with(".js"))
                .ok_or("bridge: the page names no script")?
                .to_string();
            let js = get(&script, &["Accept-Encoding: gzip"])?;
            if js.status != 200 || js.header("content-encoding") != Some("gzip") {
                return Err(format!("bridge: {script}: {} without gzip", js.status));
            }
            // Nothing without the code.
            expect_reply(&get("/api/system", &[])?, "no code", 401, &["ui pair"])?;
            let wrong = format!("Authorization: Bearer {}", "0".repeat(code.len()));
            expect_reply(&get("/api/system", &[&wrong])?, "a wrong code", 401, &[])?;
            let refused = post(
                "/api/session",
                &[],
                r#"{"token":"00000000000000000000000000000000"}"#,
            )?;
            expect_reply(&refused, "a wrong login", 401, &[])?;
            // The System API with it.
            let system = get("/api/system", &auth)?;
            expect_reply(
                &system,
                "system",
                200,
                &[
                    "\"totalMiB\":",
                    "\"freeMiB\":",
                    "\"processes\":[",
                    "\"name\":\"init\"",
                ],
            )?;
            if system.header("access-control-allow-origin").is_some() {
                return Err("bridge: an API answered with a CORS header".into());
            }
            expect_reply(
                &get("/api/apps", &auth)?,
                "apps",
                200,
                &["\"id\":\"app.oceans.hello\""],
            )?;
            expect_reply(
                &get("/api/apps/app.oceans.hello/permissions", &auth)?,
                "permissions",
                200,
                &["\"name\":\"console\""],
            )?;
            // Apps start and stop through the paired capability.
            let hello = "/api/apps/app.oceans.hello";
            let started = post(&format!("{hello}/start"), &auth, r#"{"args":"wait"}"#)?;
            expect_reply(&started, "start", 200, &["running"])?;
            let listed = get("/api/apps", &auth)?.text();
            let running = listed
                .split("\"id\":\"app.oceans.hello\"")
                .nth(1)
                .and_then(|rest| rest.split('}').next())
                .is_some_and(|fields| fields.contains("\"running\":true"));
            if !running {
                return Err(format!(
                    "bridge: hello is not running after the start: {listed}"
                ));
            }
            let stopped = post(&format!("{hello}/stop"), &auth, "{}")?;
            expect_reply(&stopped, "stop", 200, &["stopped"])?;
            let again = post(&format!("{hello}/stop"), &auth, "{}")?;
            expect_reply(&again, "stop again", 409, &["not running"])?;
            // Changes from another site's page are refused.
            let foreign = [bearer.as_str(), "Origin: http://evil.example"];
            let crossed = post(&format!("{hello}/start"), &foreign, "{}")?;
            expect_reply(&crossed, "cross-origin", 403, &[])?;
            // The login: an HttpOnly cookie that works like the code.
            let login = post("/api/session", &[], &format!(r#"{{"token":"{code}"}}"#))?;
            expect_reply(&login, "login", 204, &[])?;
            let cookie = login
                .header("set-cookie")
                .filter(|c| c.contains("HttpOnly") && c.contains("SameSite=Strict"))
                .and_then(|c| c.split(';').next())
                .ok_or("bridge: the login set no HttpOnly cookie")?
                .to_string();
            let with_cookie = format!("Cookie: {cookie}");
            expect_reply(&get("/api/audit", &[&with_cookie])?, "audit", 200, &["["])?;
            // Oceans AI from the browser: a read-only answer, then an
            // action the browser's user approves.
            let answer = post(
                "/api/ai/ask",
                &auth,
                r#"{"question":"how much memory is free?"}"#,
            )?;
            expect_reply(
                &answer,
                "ask",
                200,
                &["\"state\":\"done\"", "Memory over https: "],
            )?;
            let asked = post(
                "/api/ai/ask",
                &auth,
                r#"{"question":"start the hello app"}"#,
            )?;
            expect_reply(
                &asked,
                "ask to start",
                200,
                &[
                    "\"state\":\"needs-approval\"",
                    "start the app Hello (app.oceans.hello)",
                ],
            )?;
            let session = asked
                .text()
                .split("\"session\":")
                .nth(1)
                .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|digits| digits.parse::<u32>().ok())
                .ok_or("bridge: no session in the AI's answer")?;
            let approved = post(
                "/api/ai/continue",
                &auth,
                &format!(r#"{{"session":{session},"approve":true}}"#),
            )?;
            expect_reply(
                &approved,
                "approve",
                200,
                &["Done: started Hello (app.oceans.hello)"],
            )?;
            expect_reply(
                &get("/api/ai/activity", &auth)?,
                "activity",
                200,
                &["approved by the user"],
            )?;
            let cleanup = post(&format!("{hello}/stop"), &auth, "{}")?;
            expect_reply(&cleanup, "stop after the AI", 200, &[])?;
            Ok(())
        }
        // The Store (ADR-0061): the host's server is the store; Tiles is
        // listed as available, and installing it only proposes: the
        // desktop asks next.
        b"store" => {
            expect_reply(
                &get("/api/store", &auth)?,
                "the Store with no source",
                200,
                &["\"source\":\"\"", "\"apps\":[]"],
            )?;
            let source = format!(r#"{{"url":"http://10.0.2.2:{http_port}/store"}}"#);
            expect_reply(
                &post("/api/store/source", &auth, &source)?,
                "the Store's source",
                200,
                &["/store"],
            )?;
            expect_reply(
                &get("/api/store", &auth)?,
                "the Store",
                200,
                &["\"id\":\"app.oceans.tiles\"", "\"state\":\"available\""],
            )?;
            expect_reply(
                &post(
                    "/api/store/install",
                    &auth,
                    r#"{"id":"app.oceans.nothing"}"#,
                )?,
                "an app the Store does not list",
                404,
                &[],
            )?;
            expect_reply(
                &post("/api/store/install", &auth, r#"{"id":"app.oceans.tiles"}"#)?,
                "installing Tiles",
                202,
                &["confirm on the device"],
            )?;
            Ok(())
        }
        // A web app (ADR-0064): its page only for the paired browser,
        // sandboxed, with a token of its own; its files public; its API
        // only for its token, and only its own data.
        b"webapp" => {
            let app = |method: &str, path: &str, headers: &[&str], body: Option<&str>| {
                bridge_request(apps_port, method, path, headers, body)
            };
            expect_reply(
                &get("/api/apps", &auth)?,
                "the web app in the list",
                200,
                &["\"id\":\"app.example.notes\"", "\"runtime\":\"web\""],
            )?;
            expect_reply(
                &app("GET", "/app.example.notes/", &[], None)?,
                "the page unpaired",
                401,
                &[],
            )?;
            let login = post("/api/session", &[], &format!(r#"{{"token":"{code}"}}"#))?;
            let cookie = login
                .header("set-cookie")
                .and_then(|c| c.split(';').next())
                .ok_or("bridge: the login set no cookie")?
                .to_string();
            let with_cookie = format!("Cookie: {cookie}");
            let page = app("GET", "/app.example.notes/", &[&with_cookie], None)?;
            expect_reply(
                &page,
                "the web app's page",
                200,
                &["name=\"oceans-app-token\""],
            )?;
            let policy = page.header("content-security-policy").unwrap_or("");
            if !policy.contains("sandbox allow-scripts allow-forms")
                || policy.contains("allow-same-origin")
            {
                return Err(format!("bridge: the web app's policy is {policy:?}"));
            }
            let html = page.text();
            let token = html
                .split("name=\"oceans-app-token\" content=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .ok_or("bridge: no token in the web app's page")?
                .to_string();
            let script = html
                .split('"')
                .find(|part| {
                    part.starts_with("/app.example.notes/_app/immutable/") && part.ends_with(".js")
                })
                .ok_or("bridge: the web app's page names no script")?;
            let asset = app("GET", script, &[], None)?;
            expect_reply(&asset, "the web app's script", 200, &[])?;
            if asset.header("access-control-allow-origin") != Some("*") {
                return Err(
                    "bridge: the web app's script is not open to its sandboxed page".into(),
                );
            }
            let bearer = format!("Authorization: Bearer {token}");
            expect_reply(
                &app(
                    "PUT",
                    "/app.example.notes/api/data/note",
                    &[&bearer],
                    Some("remember the web"),
                )?,
                "storing a note",
                204,
                &[],
            )?;
            expect_reply(
                &app("GET", "/app.example.notes/api/data/note", &[&bearer], None)?,
                "the note",
                200,
                &["remember the web"],
            )?;
            expect_reply(
                &app(
                    "GET",
                    "/app.example.notes/api/data/note",
                    &[&bearer_for(code)],
                    None,
                )?,
                "the system's code as an app token",
                403,
                &[],
            )?;
            expect_reply(
                &app("GET", "/app.oceans.hello/api/data/note", &[&bearer], None)?,
                "another app's data",
                403,
                &[],
            )?;
            Ok(())
        }
        b"unpaired" => {
            expect_reply(&get("/api/system", &auth)?, "the revoked code", 401, &[])?;
            let state = get("/api/session", &[])?;
            expect_reply(&state, "the session", 200, &["\"paired\":false"])?;
            Ok(())
        }
        other => Err(format!(
            "unknown bridge step `{}`",
            String::from_utf8_lossy(other)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648() {
        for (input, output) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), output, "{input:?}");
        }
    }

    #[test]
    fn the_roots_bundle_holds_every_root() {
        let pem = ca_roots_pem().unwrap();
        let roots = oceans_tls::parse_certificates(pem.as_bytes());
        assert_eq!(roots.len(), oceans_tls::web_roots().roots.len());
        assert!(roots.len() > 100);
        for (parsed, original) in roots.iter().zip(webpki_root_certs::TLS_SERVER_ROOT_CERTS) {
            assert_eq!(parsed.as_ref(), original.as_ref());
        }
    }
}
