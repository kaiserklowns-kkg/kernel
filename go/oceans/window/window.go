// Package window is the Go side of the Oceans window protocol (ADR-0059,
// ADR-0060): an app given the `window` permission opens windows through
// its "use windows" end, draws into Pixels and presents them; the display
// service frames each window with the app's verified name and sends it
// events (keys while it has the focus, the pointer, focus changes, the
// close button).
//
// The wire format is libs/window's (oceans_window::proto); its tests and
// these check the same encodings byte for byte.
//
// Go programs run in the Go host, whose module memory cannot be shared:
// Pixels lives in Go memory, and Present copies it into the memory the
// display shares (oceans.MemoryWrite) before telling the display.
package window

import (
	"encoding/binary"
	"errors"
	"unicode"
	"unicode/utf8"
	"unsafe"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
)

// Operations (request labels).
const (
	opOpen    = 1
	opPresent = 2
	opEvents  = 3
	opClose   = 4
)

// Limits (oceans_window::proto).
const (
	MinWidth   = 64
	MinHeight  = 32
	MaxWidth   = 1024
	MaxHeight  = 768
	MaxTitle   = 48
	MaxEvents  = 20
	eventSize  = 12
	openHeader = 12
)

// Event kinds.
const (
	Key     = 1 // a key byte typed while the window had the focus
	Pointer = 2 // the pointer moved over the focused window
	Button  = 3 // a button went down or up over the focused window
	Focus   = 4 // the window gained (Pressed) or lost the focus
	Close   = 5 // the user clicked the close button
)

// Event is something that happened to a window.
type Event struct {
	Window  uint32
	Kind    uint8
	Key     byte
	Button  uint8
	Pressed bool
	X, Y    int16
}

// Encode is the event's wire form.
func (e Event) Encode() [eventSize]byte {
	var out [eventSize]byte
	binary.LittleEndian.PutUint32(out[0:], e.Window)
	out[4] = e.Kind
	out[5] = e.Key
	out[6] = e.Button
	if e.Pressed {
		out[7] = 1
	}
	binary.LittleEndian.PutUint16(out[8:], uint16(e.X))
	binary.LittleEndian.PutUint16(out[10:], uint16(e.Y))
	return out
}

// DecodeEvent reads one event; false if too short or of an unknown kind.
func DecodeEvent(b []byte) (Event, bool) {
	if len(b) < eventSize || b[4] < Key || b[4] > Close {
		return Event{}, false
	}
	return Event{
		Window:  binary.LittleEndian.Uint32(b[0:]),
		Kind:    b[4],
		Key:     b[5],
		Button:  b[6],
		Pressed: b[7] != 0,
		X:       int16(binary.LittleEndian.Uint16(b[8:])),
		Y:       int16(binary.LittleEndian.Uint16(b[10:])),
	}, true
}

// Status is a refused request's reply label.
type Status uint64

const (
	StatusBadRequest Status = 1
	StatusNotAllowed Status = 2 // Core does not know this app as one with `window`
	StatusTooMany    Status = 3
	StatusNoMemory   Status = 4
	StatusNotFound   Status = 5
)

func (s Status) Error() string {
	switch s {
	case StatusNotAllowed:
		return "window: not allowed (no window permission)"
	case StatusTooMany:
		return "window: too many windows"
	case StatusNoMemory:
		return "window: out of memory"
	case StatusNotFound:
		return "window: no such window"
	default:
		return "window: bad request"
	}
}

// ErrBadSize: the size or title is outside the limits.
var ErrBadSize = errors.New("window: size or title out of bounds")

// encodeOpen is an OPEN request's data.
func encodeOpen(bits uint64, width, height int, title string) ([]byte, error) {
	if width < MinWidth || width > MaxWidth || height < MinHeight || height > MaxHeight ||
		len(title) > MaxTitle || !utf8.ValidString(title) || bits == 0 {
		return nil, ErrBadSize
	}
	for _, r := range title {
		if unicode.IsControl(r) {
			return nil, ErrBadSize
		}
	}
	out := make([]byte, openHeader+len(title))
	binary.LittleEndian.PutUint64(out[0:], bits)
	binary.LittleEndian.PutUint16(out[8:], uint16(width))
	binary.LittleEndian.PutUint16(out[10:], uint16(height))
	copy(out[openHeader:], title)
	return out, nil
}

func check(m oceans.Message, err error) (oceans.Message, error) {
	if err != nil {
		return m, err
	}
	if m.Label != 0 {
		for _, h := range m.Handles {
			_ = oceans.Close(h)
		}
		return m, Status(m.Label)
	}
	return m, nil
}

// Window is one of this app's windows.
type Window struct {
	windows oceans.Handle
	memory  oceans.Handle
	ID      uint32
	Width   int
	Height  int
	// Pixels, row after row, each 0x00RRGGBB; shown by Present.
	Pixels []uint32
}

// Open opens a window of width × height pixels through `windows` (the
// "use windows" end); events for it signal `bits` on `notification`.
func Open(windows, notification oceans.Handle, bits uint64, width, height int, title string) (*Window, error) {
	data, err := encodeOpen(bits, width, height, title)
	if err != nil {
		return nil, err
	}
	shared, err := oceans.Duplicate(notification, oceans.RightSignal|oceans.RightTransfer)
	if err != nil {
		return nil, err
	}
	reply, err := check(oceans.Call(windows, opOpen, data, []oceans.Handle{shared}))
	if err != nil {
		return nil, err
	}
	if len(reply.Data) < 4 || len(reply.Handles) != 1 {
		for _, h := range reply.Handles {
			_ = oceans.Close(h)
		}
		return nil, StatusBadRequest
	}
	return &Window{
		windows: windows,
		memory:  reply.Handles[0],
		ID:      binary.LittleEndian.Uint32(reply.Data),
		Width:   width,
		Height:  height,
		Pixels:  make([]uint32, width*height),
	}, nil
}

func (w *Window) simple(op uint64) error {
	var id [4]byte
	binary.LittleEndian.PutUint32(id[:], w.ID)
	_, err := check(oceans.Call(w.windows, op, id[:], nil))
	return err
}

// Present shows Pixels.
func (w *Window) Present() error {
	// WebAssembly is little-endian, as the pixel format is.
	bytes := unsafe.Slice((*byte)(unsafe.Pointer(unsafe.SliceData(w.Pixels))), len(w.Pixels)*4)
	if err := oceans.MemoryWrite(w.memory, 0, bytes); err != nil {
		return err
	}
	return w.simple(opPresent)
}

// Close closes the window.
func (w *Window) Close() error {
	err := w.simple(opClose)
	_ = oceans.Close(w.memory)
	return err
}

// Events takes the queued events of this app's windows (none: empty).
func Events(windows oceans.Handle) ([]Event, error) {
	reply, err := check(oceans.Call(windows, opEvents, nil, nil))
	if err != nil {
		return nil, err
	}
	var events []Event
	for b := reply.Data; len(b) >= eventSize; b = b[eventSize:] {
		if e, ok := DecodeEvent(b); ok {
			events = append(events, e)
		}
	}
	return events, nil
}
