package window

import (
	"bytes"
	"testing"
)

// The same bytes as libs/window's `wire_format_is_fixed` test.
var (
	goldenEvent = []byte{7, 0, 0, 0, 3, 0, 1, 1, 0xfd, 0xff, 0x2c, 0x01}
	goldenOpen  = []byte{1, 0, 0, 0, 0, 0, 0, 0, 0xe0, 0x01, 0xf0, 0x00, 'N', 'o', 't', 'e', 's'}
)

func TestWireFormatMatchesRust(t *testing.T) {
	e := Event{Window: 7, Kind: Button, Button: 1, Pressed: true, X: -3, Y: 300}
	if got := e.Encode(); !bytes.Equal(got[:], goldenEvent) {
		t.Fatalf("event: % x", got)
	}
	if back, ok := DecodeEvent(goldenEvent); !ok || back != e {
		t.Fatalf("decoded %+v", back)
	}
	open, err := encodeOpen(1, 480, 240, "Notes")
	if err != nil || !bytes.Equal(open, goldenOpen) {
		t.Fatalf("open: % x %v", open, err)
	}
}

func TestUnknownEventsAreDropped(t *testing.T) {
	for _, kind := range []byte{0, 6, 255} {
		b := append([]byte(nil), goldenEvent...)
		b[4] = kind
		if _, ok := DecodeEvent(b); ok {
			t.Errorf("kind %d accepted", kind)
		}
	}
	if _, ok := DecodeEvent(goldenEvent[:11]); ok {
		t.Error("short event accepted")
	}
}

func TestOpenBounds(t *testing.T) {
	for _, c := range []struct {
		bits          uint64
		width, height int
		title         string
	}{
		{1, MaxWidth + 1, 100, ""},
		{1, 100, MinHeight - 1, ""},
		{0, 100, 100, ""},
		{1, 100, 100, "tab\there"},
		{1, 100, 100, string([]byte{0xff})},
		{1, 100, 100, "a title much longer than forty-eight bytes allows"},
	} {
		if _, err := encodeOpen(c.bits, c.width, c.height, c.title); err != ErrBadSize {
			t.Errorf("%+v: %v", c, err)
		}
	}
}
