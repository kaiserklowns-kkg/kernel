// Package fs is the Oceans file protocol (fs-proto, ADR-0019) for Go
// programs. A Node is a capability to one file or directory, and what is
// below it: paths are resolved from a directory the program was given,
// never from a global root (there is no `..`).
package fs

import (
	"encoding/binary"
	"errors"
	"strings"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
)

// fs-proto operations.
const (
	opOpen     = 1
	opRead     = 2
	opWrite    = 3
	opStat     = 4
	opList     = 5
	opRemove   = 6
	opTruncate = 7
	opSync     = 8
)

// Open flags.
const (
	CreateFile      = 1 << 0
	CreateDirectory = 1 << 1
	Write           = 1 << 2
)

// Kind of a node.
type Kind uint8

const (
	File      Kind = 1
	Directory Kind = 2
)

// Largest data per request (fs-proto MAX_DATA).
const maxData = 248

var statusText = map[uint64]string{
	1: "not found", 2: "already exists", 3: "not a directory", 4: "is a directory",
	5: "directory not empty", 6: "permission denied", 7: "invalid name", 8: "no space",
	9: "bad request", 10: "I/O error", 11: "data corrupted on disk", 12: "no disk",
	13: "not an Oceans volume", 14: "not on the same filesystem",
}

// Error is an fs-proto status.
type Error uint64

func (e Error) Error() string {
	if text, ok := statusText[uint64(e)]; ok {
		return "fs: " + text
	}
	return "fs: error"
}

// ErrNotFound is the status of a missing entry.
const ErrNotFound Error = 1

// Node is an open file or directory.
type Node struct{ handle oceans.Handle }

// FromHandle wraps a node capability from the handle directory.
func FromHandle(h oceans.Handle) Node { return Node{h} }

func (n Node) call(op uint64, data []byte) (oceans.Message, error) {
	reply, err := oceans.Call(n.handle, op, data, nil)
	if err != nil {
		return reply, err
	}
	if reply.Label != 0 {
		for _, h := range reply.Handles {
			_ = oceans.Close(h)
		}
		return reply, Error(reply.Label)
	}
	return reply, nil
}

// ValidName: what a directory entry may be called.
func ValidName(name string) bool {
	return name != "" && len(name) <= 128 && name != "." && name != ".." &&
		!strings.ContainsAny(name, "/\x00")
}

// Open opens child `name` of this directory.
func (n Node) Open(name string, flags uint8) (Node, Kind, error) {
	if !ValidName(name) {
		return Node{}, 0, Error(7)
	}
	reply, err := n.call(opOpen, append([]byte{flags}, name...))
	if err != nil {
		return Node{}, 0, err
	}
	if len(reply.Handles) != 1 || len(reply.Data) < 1 {
		for _, h := range reply.Handles {
			_ = oceans.Close(h)
		}
		return Node{}, 0, Error(9)
	}
	return Node{reply.Handles[0]}, Kind(reply.Data[0]), nil
}

// Walk resolves a relative path (components separated by `/`); `flags`
// apply to the last component, write access to the ones on the way.
func (n Node) Walk(path string, flags uint8) (Node, Kind, error) {
	var parts []string
	for _, part := range strings.Split(path, "/") {
		if part != "" {
			parts = append(parts, part)
		}
	}
	if len(parts) == 0 {
		return Node{}, 0, Error(7)
	}
	current, kind := n, Directory
	for i, part := range parts {
		if kind != Directory {
			if current != n {
				current.Close()
			}
			return Node{}, 0, Error(3)
		}
		f := flags & Write
		if i == len(parts)-1 {
			f = flags
		}
		next, nextKind, err := current.Open(part, f)
		if current != n {
			current.Close()
		}
		if err != nil {
			return Node{}, 0, err
		}
		current, kind = next, nextKind
	}
	return current, kind, nil
}

// Stat returns the kind and size.
func (n Node) Stat() (Kind, uint64, error) {
	reply, err := n.call(opStat, nil)
	if err != nil {
		return 0, 0, err
	}
	if len(reply.Data) < 10 {
		return 0, 0, Error(9)
	}
	return Kind(reply.Data[0]), binary.LittleEndian.Uint64(reply.Data[1:]), nil
}

// ReadAt reads up to len(b) bytes at `offset` (one request per 248 bytes).
func (n Node) ReadAt(b []byte, offset uint64) (int, error) {
	done := 0
	for done < len(b) {
		want := min(len(b)-done, maxData)
		data := binary.LittleEndian.AppendUint64(nil, offset+uint64(done))
		data = binary.LittleEndian.AppendUint32(data, uint32(want))
		reply, err := n.call(opRead, data)
		if err != nil {
			return done, err
		}
		done += copy(b[done:], reply.Data)
		if len(reply.Data) < want {
			break
		}
	}
	return done, nil
}

// ReadAll reads a whole file, at most `limit` bytes.
func (n Node) ReadAll(limit uint64) ([]byte, error) {
	kind, size, err := n.Stat()
	if err != nil {
		return nil, err
	}
	if kind != File {
		return nil, Error(4)
	}
	if size > limit {
		return nil, errors.New("fs: the file is too large")
	}
	b := make([]byte, size)
	got, err := n.ReadAt(b, 0)
	return b[:got], err
}

// WriteAt writes all of `b` at `offset`.
func (n Node) WriteAt(b []byte, offset uint64) error {
	for len(b) > 0 {
		take := min(len(b), maxData-8)
		data := binary.LittleEndian.AppendUint64(nil, offset)
		data = append(data, b[:take]...)
		reply, err := n.call(opWrite, data)
		if err != nil {
			return err
		}
		if len(reply.Data) < 4 {
			return Error(9)
		}
		written := int(binary.LittleEndian.Uint32(reply.Data))
		if written == 0 {
			return Error(8)
		}
		b, offset = b[written:], offset+uint64(written)
	}
	return nil
}

func (n Node) Truncate(size uint64) error {
	_, err := n.call(opTruncate, binary.LittleEndian.AppendUint64(nil, size))
	return err
}

// Sync makes every change in the filesystem durable.
func (n Node) Sync() error {
	_, err := n.call(opSync, nil)
	return err
}

// Entry is one directory entry.
type Entry struct {
	Name string
	Kind Kind
}

// List returns the entries of this directory, at most `limit`.
func (n Node) List(limit int) ([]Entry, error) {
	var entries []Entry
	for index := uint32(0); len(entries) < limit; index++ {
		reply, err := n.call(opList, binary.LittleEndian.AppendUint32(nil, index))
		if err == ErrNotFound {
			break
		}
		if err != nil {
			return entries, err
		}
		if len(reply.Data) < 1 {
			return entries, Error(9)
		}
		entries = append(entries, Entry{Name: string(reply.Data[1:]), Kind: Kind(reply.Data[0])})
	}
	return entries, nil
}

// Remove removes child `name` of this directory.
func (n Node) Remove(name string) error {
	_, err := n.call(opRemove, []byte(name))
	return err
}

// Close releases the node.
func (n Node) Close() { _ = oceans.Close(n.handle) }

// WriteFile replaces file `name` in this directory with `b`, durably.
func (n Node) WriteFile(name string, b []byte) error {
	file, kind, err := n.Open(name, CreateFile|Write)
	if err != nil {
		return err
	}
	defer file.Close()
	if kind != File {
		return Error(4)
	}
	if err := file.Truncate(0); err != nil {
		return err
	}
	if err := file.WriteAt(b, 0); err != nil {
		return err
	}
	return file.Sync()
}
