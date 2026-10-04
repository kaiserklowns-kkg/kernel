package oceans

import (
	"encoding/binary"
	"testing"
)

func TestParseAppInfo(t *testing.T) {
	app, err := ParseAppInfo("app.oceans.greeter 1.0.0")
	if err != nil || app != (App{ID: "app.oceans.greeter", Version: "1.0.0"}) {
		t.Fatalf("got %+v, %v", app, err)
	}
	for _, bad := range []string{"", "app.oceans.greeter", " 1.0.0", "app.oceans.greeter ", "a b c"} {
		if _, err := ParseAppInfo(bad); err == nil {
			t.Errorf("%q: accepted", bad)
		}
	}
}

func TestParseMemory(t *testing.T) {
	data := make([]byte, 32)
	for i, v := range []uint64{4096, 1000, 250, 12345} {
		binary.LittleEndian.PutUint64(data[8*i:], v)
	}
	m, err := ParseMemory(data)
	if err != nil {
		t.Fatal(err)
	}
	if m.TotalBytes() != 4096*1000 || m.FreeBytes() != 4096*250 || m.KernelHeap != 12345 {
		t.Fatalf("got %+v", m)
	}
	if _, err := ParseMemory(data[:31]); err == nil {
		t.Error("a short record was accepted")
	}
}
