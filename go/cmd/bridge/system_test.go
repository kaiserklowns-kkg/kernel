package main

import (
	"encoding/binary"
	"testing"
)

func record(id, parent uint64, exit int64, memory uint64, name string) []byte {
	b := make([]byte, 64)
	binary.LittleEndian.PutUint64(b, id)
	binary.LittleEndian.PutUint64(b[8:], parent)
	binary.LittleEndian.PutUint64(b[16:], uint64(exit))
	binary.LittleEndian.PutUint64(b[24:], memory)
	copy(b[32:], name)
	return b
}

func TestParsesSystemInformation(t *testing.T) {
	kernel := make([]byte, 40)
	binary.LittleEndian.PutUint64(kernel, 9)
	copy(kernel[8:], "0.1.0")
	copy(kernel[24:], "x86_64")
	if k := parseKernel(kernel); k.Version != "0.1.0" || k.Arch != "x86_64" || k.ABI != 9 {
		t.Fatalf("%+v", k)
	}
	memory := binary.LittleEndian.AppendUint64(nil, 4096)
	memory = binary.LittleEndian.AppendUint64(memory, 61440) // 240 MiB
	memory = binary.LittleEndian.AppendUint64(memory, 51200) // 200 MiB
	memory = binary.LittleEndian.AppendUint64(memory, 2048*1024)
	if m := parseMemory(memory); m.TotalMiB != 240 || m.FreeMiB != 200 || m.KernelHeapKiB != 2048 {
		t.Fatalf("%+v", m)
	}
	uptime := binary.LittleEndian.AppendUint64(nil, 2500)
	uptime = binary.LittleEndian.AppendUint64(uptime, 100)
	if s := parseUptime(uptime); s != 25 {
		t.Fatalf("uptime %d", s)
	}
	if parseUptime(make([]byte, 16)) != 0 || parseUptime(nil) != 0 || parseMemory(nil).TotalMiB != 0 {
		t.Fatal("short or zero records")
	}
	running := int64(-1 << 63)
	var procs []byte
	procs = append(procs, record(1, 0, running, 64<<10, "init")...)
	procs = append(procs, record(2, 1, 0, 1<<20, "exited")...)
	procs = append(procs, record(3, 1, running, 512<<10, "gohost")...)
	procs = append(procs, 1, 2, 3) // a torn record
	list := parseProcesses(procs)
	if len(list) != 2 || list[0].Name != "gohost" || list[0].MemoryKiB != 512 || list[1].Name != "init" {
		t.Fatalf("%+v", list)
	}
}

func TestParsesCoreReplies(t *testing.T) {
	app, ok := parseListEntry([]byte("\x01app.oceans.hello\x001.0.0\x00Hello"))
	if !ok || app.ID != "app.oceans.hello" || app.Version != "1.0.0" || app.Name != "Hello" || !app.Running {
		t.Fatalf("%+v", app)
	}
	for _, bad := range []string{"", "\x00", "\x00app\x001.0", "\x00Bad Id\x001\x00x"} {
		if _, ok := parseListEntry([]byte(bad)); ok {
			t.Errorf("%q accepted", bad)
		}
	}
	p, ok := parsePermission([]byte{0, 0})
	if !ok || p.Name != "console" || p.Decision != "automatic" {
		t.Fatalf("%+v", p)
	}
	for _, bad := range [][]byte{nil, {0}, {9, 0}, {0, 4}} {
		if _, ok := parsePermission(bad); ok {
			t.Errorf("%v accepted", bad)
		}
	}
}

func TestAppIDs(t *testing.T) {
	for id, ok := range map[string]bool{
		"app.oceans.hello": true, "app.oceans.greeter-service": true, "": false,
		"App.Hello": false, "a/b": false, "a b": false, "a\x00": false,
	} {
		if validAppID(id) != ok {
			t.Errorf("%q", id)
		}
	}
}
