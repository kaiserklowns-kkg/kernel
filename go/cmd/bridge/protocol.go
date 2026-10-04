package main

// The `bridge` protocol (ADR-0058), between the user's agent (the shell's
// `ui` command) and the bridge. Requests are IPC calls on the `bridge`
// endpoint; the reply label is a status.
//
//	PAIR    data = the token (32 to 128 hex digits), handles = [core]
//	        (a Core capability the user's agent minted for browsers)
//	        → [port u16]. Replaces an earlier pairing.
//	UNPAIR  → closes the paired capability and forgets the token;
//	        NotPaired if there was none.
//	STATUS  → [paired u8][port u16] (port 0: not listening).
const (
	opPair   = 1
	opUnpair = 2
	opStatus = 3
)

const (
	statusOK         = 0
	statusBadRequest = 1
	statusNotPaired  = 2
	// The bridge has no network listener (see the log).
	statusUnavailable = 3
)
