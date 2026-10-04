# ADR-0007: AI agents as unprivileged, mediated principals

- Status: Accepted — implemented by [ADR-0051](0051-ai-runtime.md) (tools with sensitivity classes, per-session delegated capabilities, approvals rendered from the tool, activity log) and [ADR-0055](0055-sensitive-reads.md) (sensitive reads, scoped to a delegated folder)
- Date: 2026-10-03

## Context

AI is a first-class Oceans capability, but an agent with unrestricted system
access is a security failure (master spec §24–25, §36). Agents can also be
manipulated by content they read (prompt injection), so their *intent* cannot
be trusted.

## Decision (direction)

1. The AI Runtime is an ordinary userspace service with **no ambient
   authority**. Each agent session runs with its own capability set.
2. Agents act **only through declared tools**. Each tool declares the
   permissions it needs and a sensitivity class:
   - *read-only, non-sensitive* (CPU load, process list) — allowed per session;
   - *sensitive read* (file contents, location) — user grant, scoped;
   - *state-changing* (network config, installs, settings) — explicit
     approval for each action, showing what will change and why.
3. **Approvals are rendered by the system**, not by the agent: the permission
   dialog text comes from the tool's declared action, so an agent cannot
   misdescribe what it is doing.
4. Content an agent reads (files, web pages, messages) is data, never
   authority; it cannot grant permissions.
5. Every tool call, argument summary, decision and result is recorded in an
   activity log visible in AI Center and via CLI.
6. All AI features are optional; the system and terminal work fully without
   them.

## Consequences

- Tools are the unit of security review.
- The permission broker (ADR-0006) is a dependency of the AI Runtime.
