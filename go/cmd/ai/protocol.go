package main

import "errors"

// The `ai` protocol (ADR-0051). Requests are IPC calls on the `ai`
// endpoint; the reply label is a status.
//
//	ASK       data = prompt, handles = [core] (delegated to the session)
//	          → [session u32][text length u32][first part of the text]
//	CONTINUE  data = [session u32][approve u8] → as ASK
//	TEXT      data = [session u32][offset u32] → more of the text
//	ACTIVITY  data = [index u32] → an activity entry, newest first
//	CONFIGURE data = "URL MODEL [--dns SERVER]" (an OpenAI-compatible
//	          endpoint, http:// or https://, by address or name; names
//	          resolved by SERVER, else the network's DNS server),
//	          handles = [CA] optional: a read-only memory object of PEM
//	          certificates to trust for this server (ADR-0054)
//
// The text is the answer (Done), the reason (Failed), or the approval
// question (NeedsApproval), worded by the tool.
const (
	opAsk       = 1
	opContinue  = 2
	opText      = 3
	opActivity  = 4
	opConfigure = 5
)

const (
	statusDone          = 0
	statusNeedsApproval = 1
	statusFailed        = 2
	statusNotFound      = 3
	statusBadRequest    = 4
)

// maxChunk bytes of text per reply.
const maxChunk = 240

var errNotConfigured = errors.New("no model is configured (ai model URL MODEL)")
