# ADR-0001: Monorepo and language boundaries

- Status: Accepted (amended by ADR-0105: what the device shows is native Rust; SvelteKit is the web experience only)
- Date: 2026-10-03

## Context

Oceans spans a kernel, system services, an AI runtime and user-facing apps.
Each layer has a different best-fit language, and the boundaries between
them must not erode (no Go or JS creeping into privileged code).

## Decision

1. One monorepo. Top-level directories are created only when a phase needs
   them; no placeholder directories.
2. **Rust**: kernel, HAL, drivers, Oceans Runtime, and any code that runs
   privileged or parses untrusted input on behalf of the system.
3. **Go**: system services and the AI Runtime (Phase 5+). Go code talks to the
   system only through the Oceans System API over IPC, via generated bindings.
4. **SvelteKit + TypeScript (strict)**, built with **Bun**: Settings, Store,
   AI Center, Control Center and other apps (Phase 7+). They reach the system
   through a local service bridge, never directly.
5. The system must boot, run and be administered from a CLI with the web UI
   layer absent.
6. Rust workspace: the kernel is a workspace member but not a default member,
   so `cargo build`/`cargo test` at the root work on the host; the kernel is
   built through `cargo xtask`.

## Consequences

- API definitions become the contract between languages; Phase 5 must pick an
  interface definition format and binding generator (separate ADR).
- Every language brings its own toolchain to CI.

## Open question

**Project license is undecided.** Until it is chosen, `license` stays unset in
Cargo manifests and no third-party source is copied into the tree (dependency
crates are fine: they stay under their own licenses). Candidates to evaluate:
MPL-2.0, Apache-2.0 or MIT/Apache dual. Owner: project lead.

## Alternatives considered

- Polyrepo: premature; cross-layer changes would need coordinated releases.
- Go for system services in the kernel tree: rejected by the master spec §55.
