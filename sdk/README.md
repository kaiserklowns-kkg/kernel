# The Oceans SDK

Build apps for Oceans in Rust, Go or SvelteKit, sign them with your own
developer key, and install them on an Oceans system. The SDK is described
in ADR-0062, developer keys in ADR-0063 and web apps in ADR-0064.

## What is in it

| Part | Where |
|---|---|
| Rust API (API level 1) | the `oceans-sdk` crate, [`user/sdk`](../user/sdk) |
| Go API | the packages under [`go/oceans`](../go/oceans) (`oceans`, `fs`, `tcp`, `udp`, `dns`, `window`) |
| Web apps (SvelteKit, Bun) | the `sveltekit` template and its `src/lib/oceans.js` (the app API) |
| Templates | [`sdk/templates`](templates): `rust`, `go`, `sveltekit` |
| The developer tool | `oceans`, built from [`tools/oceans`](../tools/oceans) |
| Examples | Rust: [Hello](../user/apps/hello), [Notes](../user/apps/notes) (a window). Go: [Greeter](../go/apps/greeter), [Tiles](../go/apps/tiles) (a window) |

The Rust API, by area (master spec §40):

| Area | `oceans_sdk::` | Permission |
|---|---|---|
| Application | `app`: entry point, handle directory, identity, arguments, console | `console` |
| Storage | `storage`: the app's data, the user's files | `storage`, `files` |
| Network | `network`: TCP, UDP, names | `network` |
| UI | `ui`: windows the system frames | `window` |
| System | `system`: memory, uptime | `system-info` |
| Notification | `notification`: notifications on the desktop (ADR-0065) | `notifications` (asked) |
| Permission | `permission`: the names a manifest may use | — |

The AI runtime is not open to apps at API level 1. Apps never use kernel
or service internals: only what is listed here. Go apps notify with
`window.Notify`.

## Getting started

You need this repository (the SDK), Rust (the pinned toolchain installs
itself), and Go 1.26 for Go apps or Bun 1.3 for web apps.

Build the tool once:

```bash
cargo build --release -p oceans-dev
```

The tool is then at `target/release/oceans` (`oceans.exe` on Windows).

1. **A developer key.** It signs your packages as you, the publisher:

   ```bash
   oceans keygen Your Name
   ```

   It writes `oceans-developer.key`. Keep it secret, and keep it safe:
   updates must be signed with the same key. It prints the line that makes
   an Oceans system trust you.
2. **A project from a template:**

   ```bash
   oceans new rust app.yourname.hello --publisher "Your Name"
   ```

   Or `oceans new go …`, or `oceans new sveltekit …` for a web app. The
   app's `manifest` says who it is, what it runs and the permissions it
   asks for. Oceans gives it nothing else.
3. **Build and sign:**

   ```bash
   oceans build hello --key oceans-developer.key
   ```

   The result is `hello/dist/app.yourname.hello-0.1.0.opk`.
4. **On the Oceans system, trust your key once, at its console:**

   ```text
   app trust add <KEY> Your Name
   ```

   Only the console can do this, and it is in the audit log. Add
   `--until YYYY-MM-DD` to trust it only through that day (ADR-0067).
   `app trust` lists the trusted keys; `app trust remove KEY` takes yours
   back.
5. **Install.** Either copy the package over and run `app install FILE`,
   or serve it as a store:

   ```bash
   oceans serve hello
   ```

   Then set `http://YOUR-MACHINE:8000` as the Store's URL in the Oceans
   web app. Oceans asks on its own screen before installing.
6. **Run it:** `app run app.yourname.hello`, or click it in the launcher.
   A web app opens instead from **Apps → Open** in the Oceans web
   experience.

## Web apps

A web app (`runtime = web`) is a SvelteKit app, built with Bun and signed
like any other. The bridge serves it to the paired browser at
`http://OCEANS:8081/ID/`:

- **Sandboxed and on its own origin:** no cookies, no browser storage,
  and no way into the system's pages or other apps.
- **Its data:** it keeps data through the app API in `src/lib/oceans.js`
  (`load(name)`, `save(name, text)`; at most 16 KiB per name). Its manifest
  must ask for `storage`.
- **The network:** it may also ask for `network`. Once the user allows it
  (`app grant ID network`), its page may connect to https servers
  (ADR-0066). It may ask for nothing else.
- **Not on the device yet:** it runs in the paired browser; the same app
  will run on Oceans once it has a web view (ADR-0056).

## Rules an app lives by

- It gets exactly the permissions its manifest asks for that are
  automatic, or that the user allows. When one is missing, its handle is
  absent: carry on without it.
- Its storage is its own (`storage`). The user's files need `files`, and
  the user's consent.
- A window's frame, with your app's name, is drawn by the system. You draw
  only inside it.
- Packages are checked again at every start. An app whose key is no
  longer trusted does not start.
