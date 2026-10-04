// Package oceans is the Go binding to the Oceans System API (ADR-0050).
//
// Go programs run on Oceans as WebAssembly (GOOS=wasip1) inside the Oceans
// Go host, an ordinary Oceans process. The host gives the module the
// capabilities the process was granted, and nothing else: a Go program
// reaches the system only through these calls (ADR-0001), each one an IPC
// call or a capability operation the host checks.
//
// The handle directory (ADR-0020) names what was granted: Find looks a
// capability up by kind and name ("use", "core"; "log", "log").
package oceans

import (
	"errors"
	"strconv"
)

// Handle is a capability in this process's table.
type Handle uint64

// Error is an Oceans system error (oceans_abi::Error).
type Error int64

const (
	ErrUnknownSyscall  Error = -1
	ErrInvalidHandle   Error = -2
	ErrMissingRights   Error = -3
	ErrWrongType       Error = -4
	ErrBadAddress      Error = -5
	ErrPeerClosed      Error = -6
	ErrNoReply         Error = -7
	ErrTooLarge        Error = -8
	ErrNoPendingCall   Error = -9
	ErrOutOfMemory     Error = -10
	ErrRevoked         Error = -11
	ErrInvalidArgument Error = -12
	ErrAddressInUse    Error = -13
	ErrInvalidImage    Error = -14
	ErrNotFound        Error = -15
	ErrBusy            Error = -16
)

var errorNames = map[Error]string{
	ErrUnknownSyscall:  "unknown system call",
	ErrInvalidHandle:   "invalid handle",
	ErrMissingRights:   "missing rights",
	ErrWrongType:       "wrong object type",
	ErrBadAddress:      "bad address",
	ErrPeerClosed:      "peer closed",
	ErrNoReply:         "no reply",
	ErrTooLarge:        "too large",
	ErrNoPendingCall:   "no pending call",
	ErrOutOfMemory:     "out of memory",
	ErrRevoked:         "revoked",
	ErrInvalidArgument: "invalid argument",
	ErrAddressInUse:    "address in use",
	ErrInvalidImage:    "invalid image",
	ErrNotFound:        "not found",
	ErrBusy:            "busy",
}

func (e Error) Error() string {
	if name, ok := errorNames[e]; ok {
		return "oceans: " + name
	}
	return "oceans: error " + strconv.FormatInt(int64(e), 10)
}

// ErrUnsupported: not running on Oceans (host builds, for tests).
var ErrUnsupported = errors.New("oceans: not running on Oceans")

// Rights (oceans_abi::rights), for Duplicate.
const (
	RightRead      uint32 = 1 << 0
	RightWrite     uint32 = 1 << 1
	RightMap       uint32 = 1 << 3
	RightSend      uint32 = 1 << 4
	RightReceive   uint32 = 1 << 5
	RightSignal    uint32 = 1 << 6
	RightWait      uint32 = 1 << 7
	RightManage    uint32 = 1 << 8
	RightDuplicate uint32 = 1 << 9
	RightTransfer  uint32 = 1 << 10
)

// Limits of one IPC message (ADR-0013).
const (
	MaxData    = 256
	MaxHandles = 4
)

// Message is what a call returns or a receive delivers.
type Message struct {
	Label   uint64
	Data    []byte
	Handles []Handle
	// Receive only: the badge of the client end used (0 = unbadged).
	Badge uint64
	// Receive only: not a call but the close of badged end Badge.
	Closed bool
	// Receive only: not a call but these bits of the bound notification.
	Signals uint64
}

func result(value int64) (int64, error) {
	if value < 0 {
		return 0, Error(value)
	}
	return value, nil
}
