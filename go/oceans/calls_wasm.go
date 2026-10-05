//go:build wasip1

package oceans

import (
	"encoding/binary"
	"unsafe"
)

// Find returns the capability of `kind` called `name` in the handle
// directory.
func Find(kind, name string) (Handle, bool) {
	h := hostHandleFind(strPtr(kind), uint32(len(kind)), strPtr(name), uint32(len(name)))
	if h < 0 {
		return 0, false
	}
	return Handle(h), true
}

// Args is the argument text the program was started with.
func Args() string {
	buf := make([]byte, 1024)
	n := hostArgs(ptr(buf), uint32(len(buf)))
	if n <= 0 {
		return ""
	}
	return string(buf[:n])
}

// DebugWrite writes one line to a log capability.
func DebugWrite(log Handle, text string) error {
	_, err := result(hostDebugWrite(uint64(log), strPtr(text), uint32(len(text))))
	return err
}

func Close(h Handle) error {
	_, err := result(hostClose(uint64(h)))
	return err
}

// Duplicate returns a new handle to the same object with `rights` (a
// subset of h's).
func Duplicate(h Handle, rights uint32) (Handle, error) {
	v, err := result(hostDuplicate(uint64(h), rights))
	return Handle(v), err
}

// Call sends a request on client end `client` and waits for the reply.
// Handles sent move to the receiver.
func Call(client Handle, label uint64, data []byte, handles []Handle) (Message, error) {
	reply := make([]byte, MaxData)
	replyHandles := make([]Handle, MaxHandles)
	var res [16]byte
	_, err := result(hostCall(uint64(client), label, ptr(data), uint32(len(data)),
		handlesPtr(handles), uint32(len(handles)), ptr(reply), uint32(len(reply)),
		handlesPtr(replyHandles), uint32(len(replyHandles)), unsafe.Pointer(&res[0])))
	if err != nil {
		return Message{}, err
	}
	n := binary.LittleEndian.Uint32(res[8:])
	h := binary.LittleEndian.Uint32(res[12:])
	return Message{
		Label:   binary.LittleEndian.Uint64(res[:8]),
		Data:    reply[:n],
		Handles: replyHandles[:h],
	}, nil
}

// Receive waits on server end `server` for a call, the close of a badged
// end, or (if a notification is bound) its signals.
func Receive(server Handle) (Message, error) {
	data := make([]byte, MaxData)
	handles := make([]Handle, MaxHandles)
	var res [40]byte
	_, err := result(hostReceive(uint64(server), ptr(data), uint32(len(data)),
		handlesPtr(handles), uint32(len(handles)), unsafe.Pointer(&res[0])))
	if err != nil {
		return Message{}, err
	}
	n := binary.LittleEndian.Uint32(res[8:])
	h := binary.LittleEndian.Uint32(res[12:])
	return Message{
		Label:   binary.LittleEndian.Uint64(res[:8]),
		Data:    data[:n],
		Handles: handles[:h],
		Badge:   binary.LittleEndian.Uint64(res[16:]),
		Closed:  binary.LittleEndian.Uint64(res[24:]) != 0,
		Signals: binary.LittleEndian.Uint64(res[32:]),
	}, nil
}

// Reply answers the call last received.
func Reply(label uint64, data []byte, handles []Handle) error {
	_, err := result(hostReply(label, ptr(data), uint32(len(data)), handlesPtr(handles), uint32(len(handles))))
	return err
}

// Mint returns a new client end of server end `server` carrying `badge`.
func Mint(server Handle, badge uint64) (Handle, error) {
	v, err := result(hostMint(uint64(server), badge))
	return Handle(v), err
}

func NotificationCreate() (Handle, error) {
	v, err := result(hostNotificationCreate())
	return Handle(v), err
}

// NotificationWait blocks until a bit is set on `notification`; returns
// and clears the bits.
func NotificationWait(notification Handle) (uint64, error) {
	v, err := result(hostNotificationWait(uint64(notification)))
	return uint64(v), err
}

// EndpointBind makes `notification`'s signals arrive through Receive on
// `server`.
func EndpointBind(server, notification Handle) error {
	_, err := result(hostEndpointBind(uint64(server), uint64(notification)))
	return err
}

// TimerSet signals `bits` on `notification` after `ms` (0 cancels).
func TimerSet(notification Handle, bits, ms uint64) error {
	_, err := result(hostTimerSet(uint64(notification), bits, ms))
	return err
}

// PublishText returns a read-only memory object holding `text` (for
// handing text to another process).
func PublishText(text string) (Handle, error) {
	v, err := result(hostPublishText(strPtr(text), uint32(len(text))))
	return Handle(v), err
}

// ReadText returns the text in read-only memory object `memory` (as
// PublishText makes them: UTF-8, up to the first NUL), at most MaxText
// bytes.
func ReadText(memory Handle) (string, error) {
	for _, size := range []int{1024, MaxText} {
		buf := make([]byte, size)
		n, err := result(hostReadText(uint64(memory), ptr(buf), uint32(len(buf))))
		if err == ErrTooLarge && size < MaxText {
			continue
		}
		if err != nil {
			return "", err
		}
		return string(buf[:n]), nil
	}
	return "", ErrTooLarge
}

// SystemInfo reads records of `kind` (oceans_abi::sysinfo) into a buffer.
func SystemInfo(sysinfo Handle, kind uint64) ([]byte, error) {
	buf := make([]byte, 16*1024)
	n, err := result(hostSystemInfo(uint64(sysinfo), kind, ptr(buf), uint32(len(buf))))
	if err != nil {
		return nil, err
	}
	return buf[:n], nil
}

// MemorySize is the size of memory object `memory` (whole pages).
func MemorySize(memory Handle) (uint64, error) {
	v, err := result(hostMemorySize(uint64(memory)))
	return uint64(v), err
}

// MemoryRead copies bytes of memory object `memory` from `offset` into
// buf (the handle needs READ and MAP); returns how many, 0 at the end.
func MemoryRead(memory Handle, offset uint64, buf []byte) (int, error) {
	n, err := result(hostMemoryRead(uint64(memory), offset, ptr(buf), uint32(len(buf))))
	return int(n), err
}

// MemoryWrite copies buf into memory object `memory` from `offset` (the
// handle needs write and map rights): all of it, or nothing if it does not
// fit (ADR-0060).
func MemoryWrite(memory Handle, offset uint64, buf []byte) error {
	_, err := result(hostMemoryWrite(uint64(memory), offset, ptr(buf), uint32(len(buf))))
	return err
}

// MemoryCreate makes a zero-filled memory object of at least `size` bytes
// (at most 16 MiB): a shared buffer to attach to a service (ADR-0030).
func MemoryCreate(size uint64) (Handle, error) {
	h, err := result(hostMemoryCreate(size))
	return Handle(h), err
}
