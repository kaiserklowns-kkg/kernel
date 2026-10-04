# ADR-0055: Sensitive reads by AI agents

- Status: Accepted
- Date: 2026-10-04
- Depends on: ADR-0007 (AI mediation), ADR-0051 (the AI runtime),
  ADR-0053 (the Go file binding)

## Context

ADR-0007 sorts what an agent may do into three classes, and ADR-0051
built two of them:
- **read-only, non-sensitive** (memory, processes, apps): runs freely;
- **state-changing** (start, stop): approved per action.

The third, **sensitive reads** (file contents and the like), was still
missing. It is what makes the master spec's examples work: "search
files", or "understand the project". Reading a person's files is not
harmless, even though nothing changes: what is read leaves through the
model.

## Decision

- **A new sensitivity, `Reads`.** A `Reads` tool needs the user's
  approval for **each call**, worded with the exact path ("read the file
  notes.txt in your folder"). It is recorded in the activity log like a
  change.
- **Scope:** the session reaches only a folder **the requester
  delegated**:
  - The shell opens the user's folder (`/home`) **read-only** and sends
    it with each question, next to the Core capability (`ASK` handles:
    `[core, files]`).
  - The agent's tools resolve paths inside it only. The handle has no
    parent, and names are checked (no `..`, no empty parts).
  - The capability is closed when the session ends.
- **Tools:**

  | Tool | Does | Limits |
  |---|---|---|
  | `files_list` | names in the folder (or a folder inside it) | at most 100 |
  | `files_read` | a text file | at most 64 KiB read, 4 000 characters to the model; binary files refused |

  Without a delegated folder they refuse before asking anything.
- **Denials** are reported to the model like denied changes. Nothing is
  read.

## Consequences

- **Agents can work with the user's files under the user's eye.** Every
  read is approved and logged. What can be read is bounded by what the
  requester chose to delegate (today the user's folder, read-only). An
  agent cannot write files: no tool does, and the handle is read-only.
- **Prompt fatigue:** approving every read can be tiring. "Allow reads
  in this folder for this session" is a natural next step, still
  per-session and logged. It needs the approval UI of Phase 7 (AI
  Center).
- **Content is data** (ADR-0007): instructions inside a file read by the
  agent cannot approve anything. Approvals come only from the
  requester's `CONTINUE`.

## Alternatives considered

- **Treating file reads as read-only (no approval):** the user would not
  know what left their machine through the model.
- **Giving the AI service the filesystem:** standing access to every
  file, for every session and every requester.
- **Approval per session for the whole folder:** fewer prompts, but less
  visible, and a session can run long. Left for the AI Center, with the
  user choosing.

## Checklist (master spec §48)

- **Purpose:** agents reading the user's files with consent.
- **Architecture:**
  - `agent.Reads`;
  - `files_list` and `files_read`;
  - the delegated read-only folder (`[core, files]`).
- **API:** the tool names and parameters (`path` inside the folder).
- **Dependencies:** `go/oceans/fs` (ADR-0053).
- **Security:**
  - per-call approval with the exact path;
  - read-only delegation of one folder;
  - path validation;
  - size limits;
  - the activity log.
- **Testing:**
  - **Go unit tests:** path validation; tools are `Reads`; they refuse
    without a folder.
  - **Smoke:** the shell writes `/home/notes.txt`; the agent asks to read
    it, it is approved, and it answers from its contents; the activity
    log shows the approved read.
- **Failure behaviour:**
  - bad paths and missing folders are refused before asking;
  - missing files and binary files are reported to the model;
  - denials read nothing.
