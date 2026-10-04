// Package tcp is a TCP client over the Oceans network service (net-proto,
// ADR-0023, ADR-0024) for Go programs: Dial returns a connection that is
// an io.ReadWriteCloser, with time limits.
package tcp

import (
	"encoding/binary"
	"errors"
	"io"
	"time"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
)

// net-proto operations and constants used here.
const (
	opTCPConnect  = 6
	opTCPSend     = 9
	opTCPRecv     = 10
	opTCPShutdown = 11
	opTCPStatus   = 12

	readable   = 1 << 0
	timeoutBit = 1 << 1

	stateSynSent     = 2
	stateSynReceived = 3
	stateEstablished = 4
	stateCloseWait   = 7

	statusEmpty = 6
	statusEOF   = 14

	maxStream = 248
)

var statusText = map[uint64]string{
	1: "bad request", 2: "network not configured yet", 3: "port in use", 4: "no route to host",
	5: "too large", 6: "nothing received", 7: "out of buffers", 8: "no network device",
	9: "connection refused", 10: "connection reset by peer", 11: "timed out",
	12: "not connected", 13: "connection closed", 14: "end of stream",
}

// Error is a net-proto status.
type Error uint64

func (e Error) Error() string {
	if text, ok := statusText[uint64(e)]; ok {
		return "tcp: " + text
	}
	return "tcp: network error"
}

// ErrTimeout: a time limit passed.
var ErrTimeout = errors.New("tcp: timed out")

// Conn is a TCP connection.
type Conn struct {
	handle       oceans.Handle
	notification oceans.Handle
	// Timeout bounds each wait in Read and Write.
	Timeout time.Duration
	pending []byte
	eof     bool
}

// Dial connects to `address:port` through the network service `net`,
// waiting up to `timeout` for the connection.
func Dial(net oceans.Handle, address [4]byte, port uint16, timeout time.Duration) (*Conn, error) {
	notification, err := oceans.NotificationCreate()
	if err != nil {
		return nil, err
	}
	shared, err := oceans.Duplicate(notification, oceans.RightSignal|oceans.RightTransfer)
	if err != nil {
		_ = oceans.Close(notification)
		return nil, err
	}
	data := make([]byte, 14)
	copy(data, address[:])
	binary.LittleEndian.PutUint16(data[4:], port)
	binary.LittleEndian.PutUint64(data[6:], readable)
	reply, err := oceans.Call(net, opTCPConnect, data, []oceans.Handle{shared})
	if err == nil && reply.Label != 0 {
		err = Error(reply.Label)
	}
	if err == nil && len(reply.Handles) != 1 {
		err = Error(1)
	}
	if err != nil {
		_ = oceans.Close(notification)
		return nil, err
	}
	c := &Conn{handle: reply.Handles[0], notification: notification, Timeout: timeout}
	if err := c.wait(timeout, c.connected); err != nil {
		c.Close()
		return nil, err
	}
	return c, nil
}

// connected: done (true), still connecting (false), or failed.
func (c *Conn) connected() (bool, error) {
	reply, err := oceans.Call(c.handle, opTCPStatus, nil, nil)
	if err != nil {
		return false, err
	}
	if reply.Label != 0 {
		return false, Error(reply.Label)
	}
	if len(reply.Data) < 2 {
		return false, Error(1)
	}
	if reply.Data[1] != 0 {
		return false, Error(reply.Data[1])
	}
	switch reply.Data[0] {
	case stateEstablished, stateCloseWait:
		return true, nil
	case stateSynSent, stateSynReceived:
		return false, nil
	}
	return false, Error(12)
}

// wait calls `done` until it says yes, sleeping on the notification, for
// at most `limit`.
func (c *Conn) wait(limit time.Duration, done func() (bool, error)) error {
	ms := uint64(limit / time.Millisecond)
	if ms == 0 {
		ms = 1
	}
	_ = oceans.TimerSet(c.notification, timeoutBit, ms)
	defer oceans.TimerSet(c.notification, timeoutBit, 0)
	for {
		ok, err := done()
		if err != nil || ok {
			return err
		}
		bits, err := oceans.NotificationWait(c.notification)
		if err != nil {
			return err
		}
		if bits&timeoutBit != 0 {
			if ok, err := done(); ok || err != nil {
				return err
			}
			return ErrTimeout
		}
	}
}

// Write sends all of `b`, waiting while the send buffer is full.
func (c *Conn) Write(b []byte) (int, error) {
	written := 0
	for written < len(b) {
		chunk := b[written:min(len(b), written+maxStream)]
		var accepted int
		err := c.wait(c.Timeout, func() (bool, error) {
			reply, err := oceans.Call(c.handle, opTCPSend, chunk, nil)
			if err != nil {
				return false, err
			}
			if reply.Label != 0 {
				return false, Error(reply.Label)
			}
			if len(reply.Data) < 4 {
				return false, Error(1)
			}
			accepted = int(binary.LittleEndian.Uint32(reply.Data))
			return accepted > 0, nil
		})
		if err != nil {
			return written, err
		}
		written += accepted
	}
	return written, nil
}

// Read reads what has arrived, waiting for something or the end.
func (c *Conn) Read(b []byte) (int, error) {
	if len(c.pending) == 0 && !c.eof {
		err := c.wait(c.Timeout, func() (bool, error) {
			reply, err := oceans.Call(c.handle, opTCPRecv, nil, nil)
			if err != nil {
				return false, err
			}
			switch reply.Label {
			case 0:
				c.pending = append(c.pending, reply.Data...)
				return true, nil
			case statusEmpty:
				return false, nil
			case statusEOF:
				c.eof = true
				return true, nil
			}
			return false, Error(reply.Label)
		})
		if err != nil {
			return 0, err
		}
	}
	if len(c.pending) == 0 {
		return 0, io.EOF
	}
	n := copy(b, c.pending)
	c.pending = c.pending[n:]
	return n, nil
}

// CloseWrite sends our end of stream.
func (c *Conn) CloseWrite() error {
	reply, err := oceans.Call(c.handle, opTCPShutdown, nil, nil)
	if err == nil && reply.Label != 0 {
		err = Error(reply.Label)
	}
	return err
}

// Close ends the connection.
func (c *Conn) Close() error {
	err := oceans.Close(c.handle)
	_ = oceans.Close(c.notification)
	return err
}
