# ADR-0056: UI architecture: a native desktop, and SvelteKit apps through a bridge

- Status: Accepted (amended by ADR-0105: no on-device web view is planned; the device shows native apps)
- Date: 2026-10-04
- Depends on: ADR-0001 (language boundaries), ADR-0029 (framebuffer
  console), ADR-0042 (pointer input), ADR-0045 to ADR-0048 (Oceans Core,
  permissions, delegation), ADR-0051 (AI runtime)
- Starts Phase 7. Details: ADR-0057 (display and desktop), ADR-0058 (web
  experience and bridge).

## Context

The master spec asks for a modern, calm, dark-first UI, with Settings,
Store, AI Center, Control Center and permission dialogs. SvelteKit builds
the experience and Bun the tooling (§6–7, §29–37). It also asks that:
- SvelteKit is never required for the system to work, and the OS stays
  usable without the web UI;
- UI comes only after the system API is defined (§55).

The system API exists now (Phases 5–6).

**Oceans has no browser engine.** A SvelteKit app needs HTML, CSS and
JavaScript. Engines that render them are millions of lines and assume a
POSIX system. Porting one is a project of its own, not a step in Phase 7.

**Permission dialogs carry the security model** (ADR-0007, ADR-0047).
What the user approves must be drawn by the system, out of reach of
anything the dialog is about.

## Decision

The UI has two layers.

- **The native layer** (Rust, on the device; ADR-0057):
  - **The display service** takes the framebuffer over from the kernel
    console, composes the screen and routes the pointer.
  - **The desktop** is drawn by that service: the Oceans Bar, the
    launcher, notifications, a Terminal showing the console, and the
    **permission dialogs**.
  - **Permission UI is system-rendered**, never a web view, and works
    with no web layer at all.
  - **If the display service stops**, the kernel console takes the screen
    back.
- **The web layer** (SvelteKit + TypeScript, built with Bun; ADR-0058):
  - **The apps:** Settings, Store, AI Center and Control Center, a static
    bundle.
  - **The bridge:** a Go service on Oceans serves them, with JSON
    endpoints over the System API.
  - **Authority:** the bridge holds none of its own. The user's agent
    pairs it and delegates narrow capabilities (ADR-0048), as the shell
    does for AI sessions.
  - **Today** the web layer is used from a browser on another device.
    **On the device** it will be shown once Oceans has an HTML engine,
    or a minimal one for these apps; a later decision.
- **Order of work:** first the native layer and the bridge, which make
  the system usable and controllable; then the on-device web view.

## Consequences

- **What works now:**
  - a desktop on the screen, with a launcher and consent by dialog;
  - the full experience (SvelteKit) from any browser once paired.
- **Nothing in the trust path depends on the web layer:**
  - permission dialogs are native;
  - the bridge cannot `decide` on its own;
  - the shell and terminal stay complete (§38: usable without AI and
    without UI).
- **What is two-tier for now:** the web apps are not yet shown on the
  device itself. That is the largest open item of Phase 7.

## Alternatives considered

- **Port a browser engine now:** months of work, and a huge attack
  surface, before anything is usable.
- **Write the whole UI natively in Rust and drop SvelteKit:** this
  contradicts the language plan (ADR-0001, §6) and the design-system
  work the web layer brings.
- **Permission dialogs in the web layer:** a compromised page or bridge
  could then draw or answer them.

## Checklist (master spec §48)

- **Purpose:** the architecture of Phase 7.
- **Architecture:**
  - native display service, desktop and dialogs;
  - SvelteKit apps through the Go bridge with delegated authority.
- **API:** ABI 13 display calls (ADR-0057); the bridge's HTTP API
  (ADR-0058).
- **Dependencies:** see ADR-0057 and ADR-0058.
- **Security:**
  - consent stays native;
  - the web layer gets only delegated, narrow capabilities;
  - the console is the fallback.
- **Testing:** in ADR-0057 and ADR-0058 (screen captures, pointer clicks,
  HTTP from the host).
- **Failure behaviour:**
  - the desktop dies: the console returns;
  - the bridge dies: the native layer is unaffected.
