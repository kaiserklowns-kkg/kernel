# ADR-0051: The AI runtime: agents, tools, approvals and the model gateway

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0007 (AI agents as mediated principals), ADR-0045
  (Oceans Core), ADR-0048 (delegating Core authority), ADR-0050 (Go on
  Oceans)
- Phase 6 exit criterion: an agent completes a permission-gated task

## Context

The master spec makes AI a system capability, not a chatbot. AI must
understand the system and act on it, but **never with unrestricted
access**: every sensitive action passes through permission controls and
the user's approval, and AI activity must be visible (§24–26, §35–36,
§50).

ADR-0007 set the direction and was Proposed until now:
- agents are unprivileged principals that act only through declared
  tools;
- approvals are rendered by the system;
- content is never authority;
- everything is logged.

Phase 6 needs the parts that make this real:
- an AI service in Go (ADR-0050);
- a model gateway;
- the agent loop;
- tools;
- the approval flow.

## Decision

### The `ai` service

The `ai` service is a Go program (`go/cmd/ai`) run by the Go host and
started by init.

- **Its own authority is small:**
  - a log;
  - the network, for the model server;
  - read-only system information.

  It holds no Core capability, no filesystem and no console.
- **Per-session authority** (ADR-0007, "each session runs with its own
  capability set"):
  - The requester delegates to each request what its agent may use,
    attached to `ASK`.
  - The shell mints a Core capability limited to **query and run**
    (ADR-0048) for every question. An agent can never `decide`, so it can
    never answer consent, grant permissions or install software.
  - The session's capabilities are closed when it ends.

### Tools

Tools (`go/ai/tools`) are the unit of security review. Each declares:
- its JSON-schema parameters, for the model;
- its **sensitivity**;
- **`Describe`**: checks the arguments and says, in the system's words,
  what running the tool will do;
- **`Run`**: the action, through the session's capabilities only.

| Tool | Sensitivity | Uses |
|---|---|---|
| `system_memory` | read-only | sysinfo |
| `system_processes` | read-only | sysinfo |
| `apps_list` | read-only | session Core (`query`) |
| `apps_start` | **changes** | session Core (`run`) |
| `apps_stop` | **changes** | session Core (`run`) |

- **Read-only tools run without asking.** They inspect non-sensitive
  state (ADR-0007's first class).
- **Tools that change something stop the session.** `ASK` (or
  `CONTINUE`) answers `NeedsApproval` with the tool's description, e.g.
  "start the app Hello (app.oceans.hello) with the arguments "wait"". The
  description comes from the tool, with the app's real name read from
  Core, not from the model.
- **Only the requester's `CONTINUE`** with the user's answer runs it.
  - A denial is reported to the model as such.
  - A refused or malformed call (unknown tool, bad arguments, an app
    that is not installed) is never run, and is reported to the model.

### The agent loop (`go/ai/agent`)

- **The conversation:** a system prompt, the user's question, then model
  replies and tool results, at most 8 model steps per session.
- **Sessions:** at most 8 are open, and the oldest is dropped.
- **The activity log** records every question, tool call (with its
  arguments), decision ("approved by the user", "denied by the user",
  "read-only", "refused") and result. The service keeps the latest 64
  for `ACTIVITY` and writes all of them to the system log.

### The model gateway (`go/ai/model`, `go/ai/httpc`, `go/oceans/tcp`)

- **The protocol:** the **OpenAI-compatible Chat Completions** API with
  tool calls, the common interface of local model servers (Ollama,
  llama.cpp) and hosted providers.
- **Transport:** HTTP/1.1 over Oceans TCP, through the binding (one
  connection per request; Content-Length, chunked or close-delimited
  responses).
- **Configuration:** `ai model URL MODEL` (`CONFIGURE`), e.g. `ai model
  http://192.168.1.10:11434/v1 llama3.1` for a local Ollama.
- **Limits of this version:**
  - `http://` with an IPv4 address only; names and `https://` need DNS
    and TLS in the gateway;
  - the setting lasts until the service restarts.
- **Content is never authority** (ADR-0007): model output and tool
  results are data. Only the requester's `CONTINUE` can make a changing
  tool run.

### The `ai` protocol (`go/cmd/ai/protocol.go`)

| Request | Data | Reply |
|---|---|---|
| `ASK` | the question, handles = delegated | `[session][length][text…]` |
| `CONTINUE` | `[session][approve]` | as `ASK` |
| `TEXT` | `[session][offset]` | more text |
| `ACTIVITY` | `[index]` | an entry, newest first |
| `CONFIGURE` | `URL MODEL` | — |

Statuses: Done, NeedsApproval, Failed, NotFound, BadRequest.

### The shell

The shell is the requester and the approval agent:
- **`ai ask QUESTION`** mints the session's Core capability. When asked,
  it shows "Oceans AI wants to: …" and `Allow? [y/N]`; the default is no.
- **`ai activity`** shows what the AI did and what the user decided.
- **`ai model URL MODEL`** sets the model server.

### Testing without a model

The smoke test's host runs a scripted model server: an OpenAI-compatible
endpoint that answers by the question and the number of tool results so
far, and quotes the last result back. It tests:
- the agent loop;
- the tools;
- the approval flow;
- the refusals.

These are deterministic, with no model and no network beyond the host.
Unit tests (`go test`) cover the agent loop:
- read-only calls;
- approvals and denials;
- bad calls;
- the step bound;
- model errors;
- session eviction.

They also cover the gateway's request and response handling and the HTTP
client.

## Consequences

- **Phase 6's exit criterion holds:** the smoke test's agent answers
  from a read-only tool, starts an app only after the user approves, and
  leaves an app running when the user denies stopping it. All of it is
  in the activity log.
- **ADR-0007 is now Accepted** and implemented by this design.
- **Adding a capability to the AI** means:
  1. a tool with its sensitivity and its system-worded description;
  2. what the requester must delegate for it.

  The review question is always the same: what can this tool do with
  what it is given?
- **Not yet:**
  - **AI Center (Phase 7):** a richer approval UI and live activity.
  - **Agents started by other agents, and long-running tasks.**
  - **Sensitive reads** (file contents) with scoped grants: ADR-0007's
    second class.
  - **https and DNS in the gateway; streaming answers.**
  - **Local inference on Oceans itself:** this needs measurements on
    real hardware first (§51).

## Alternatives considered

- **Giving the AI service its own Core capability:** an agent would hold
  standing authority between requests, and every requester would share
  it. Per-session delegation ties what an agent can do to who asked, and
  to that request alone.
- **Approvals inside the AI service** (it asks the user itself): the
  service would need the console, and the model's process would also
  render the question. The requester's agent shows it; the AI service
  only waits.
- **A model-specific API:** the OpenAI-compatible interface is what
  local servers and most providers speak. Another provider is another
  `model.Model`.

## Checklist (master spec §48)

- **Purpose:** AI agents that understand and act on the system under the
  user's control.
- **Architecture:**
  - the `ai` Go service (agent runtime, tools, gateway);
  - per-session delegated capabilities;
  - approvals through the requester;
  - the activity log.
- **API:**
  - the `ai` protocol;
  - `agent.Tool` and the sensitivity classes;
  - the shell's `ai` command.
- **Dependencies:** Go's standard library (`encoding/json`, `strings`, …)
  only; an OpenAI-compatible model server, chosen by the user.
- **Security:**
  - no standing authority beyond network and read-only information;
  - sessions get `query`+`run` at most, never `decide`;
  - changes need per-action approval worded by the tool;
  - model output is data;
  - everything is logged;
  - the AI is optional: the system works fully without it (§24, §38).
- **Testing:**
  - **Go unit tests:** agent, model, HTTP client.
  - **Smoke, with the scripted model server:**
    - unconfigured;
    - configured;
    - a read-only answer;
    - an approved start;
    - a denied stop;
    - the activity entries.
- **Failure behaviour:**
  - model and network errors fail the session with the reason;
  - bad tool calls are refused and not run;
  - unanswered sessions are bounded and dropped;
  - closing the session drops its capabilities.
