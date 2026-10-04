package oceans

import (
	"encoding/binary"
	"errors"
	"strings"
)

// MaxText is the most ReadText returns: published text is small (an
// app's identity, its arguments, a handle directory).
const MaxText = 64 * 1024

// App is what an app installed from a package (ADR-0046, ADR-0052) knows
// about itself: Oceans Core grants it as "app info", a text object
// holding "ID VERSION".
type App struct {
	ID      string
	Version string
}

// ErrNotAnApp: the process has no "app info" (it was not started by
// Oceans Core from a package).
var ErrNotAnApp = errors.New("oceans: not started as an app (no app info)")

// AppInfo returns this app's identity.
func AppInfo() (App, error) {
	info, ok := Find("app", "info")
	if !ok {
		return App{}, ErrNotAnApp
	}
	text, err := ReadText(info)
	if err != nil {
		return App{}, err
	}
	return ParseAppInfo(text)
}

// ParseAppInfo parses the text of "app info": an id and a version,
// separated by one space.
func ParseAppInfo(text string) (App, error) {
	id, version, ok := strings.Cut(strings.TrimSpace(text), " ")
	if !ok || id == "" || version == "" || strings.ContainsAny(version, " \t\n") {
		return App{}, errors.New("oceans: app info is not \"ID VERSION\"")
	}
	return App{ID: id, Version: version}, nil
}

// SysinfoMemory is the system-information kind of Memory's record
// (oceans_abi::sysinfo::MEMORY).
const SysinfoMemory = 1

// MemoryInfo is the system's memory (oceans_abi::sysinfo::MemoryInfo).
type MemoryInfo struct {
	PageSize    uint64
	TotalFrames uint64
	FreeFrames  uint64
	// Kernel heap bytes in use.
	KernelHeap uint64
}

// TotalBytes is the memory the kernel manages.
func (m MemoryInfo) TotalBytes() uint64 { return m.TotalFrames * m.PageSize }

// FreeBytes is the memory not in use.
func (m MemoryInfo) FreeBytes() uint64 { return m.FreeFrames * m.PageSize }

// Memory reads the memory figures through a "sysinfo" capability.
func Memory(sysinfo Handle) (MemoryInfo, error) {
	data, err := SystemInfo(sysinfo, SysinfoMemory)
	if err != nil {
		return MemoryInfo{}, err
	}
	return ParseMemory(data)
}

// ParseMemory decodes a memory record (32 bytes, little-endian words).
func ParseMemory(data []byte) (MemoryInfo, error) {
	if len(data) < 32 {
		return MemoryInfo{}, errors.New("oceans: short memory record")
	}
	word := func(i int) uint64 { return binary.LittleEndian.Uint64(data[8*i:]) }
	return MemoryInfo{
		PageSize:    word(0),
		TotalFrames: word(1),
		FreeFrames:  word(2),
		KernelHeap:  word(3),
	}, nil
}
