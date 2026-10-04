package main

import (
	"encoding/binary"
	"errors"
	"sort"
	"strings"

	"github.com/kaiserklowns-kkg/kernel/go/oceans"
)

// The System API on Oceans: read-only system information (the bridge's
// own grant), Oceans Core through the paired capability (ADR-0048), and
// the AI runtime (ADR-0051).

// sysinfo kinds (oceans_abi::sysinfo).
const (
	sysinfoKernel    = 0
	sysinfoMemory    = 1
	sysinfoUptime    = 2
	sysinfoProcesses = 3
)

// Oceans Core (user/core-proto).
const (
	coreList       = 2
	coreInfo       = 3
	corePermission = 4
	coreRun        = 5
	coreStop       = 6
	coreAudit      = 10
	coreMint       = 11

	accessQuery = 1 << 0
	accessRun   = 1 << 1
	accessAudit = 1 << 4

	runDetach = 1 << 0

	fieldVersion   = 1
	fieldPublisher = 2
	fieldKind      = 9
	fieldRuntime   = 10
)

// What the paired capability may do: look, run and stop installed apps,
// read the audit log. Never install, remove or decide permissions.
const pairedRights = accessQuery | accessRun | accessAudit

// coreErrors: Core statuses as HTTP statuses and the user's words.
var coreErrors = map[uint64]*apiError{
	1:  {404, "that app is not installed"},
	2:  {400, "Oceans Core refused the request"},
	4:  {409, "the app needs a permission you have not decided yet: run it once on the Oceans console to answer"},
	5:  {409, "the app is not running"},
	9:  {409, "the app is already running"},
	10: {500, "storage failed"},
	11: {500, "the app cannot be started"},
	12: {403, "pairing does not allow this"},
	13: {409, "not a service"},
}

// The permissions (oceans_package::Permission::ALL, in order) and what
// the system says they mean, as libs/package has them (its tests check
// this list).
var permissions = []struct{ name, description string }{
	{"console", "write to the terminal that started it"},
	{"storage", "keep its own data"},
	{"system-info", "see processes, memory use and uptime"},
	{"network", "connect to the internet and the local network"},
	{"files", "read and change your files in /home"},
	{"pointer", "see your mouse and tablet movements and clicks"},
	{"window", "show windows, and get what you type into them"},
}

var decisions = []string{"automatic", "allowed", "denied", "undecided"}

// The `ai` protocol (go/cmd/ai/protocol.go).
const (
	aiAsk       = 1
	aiContinue  = 2
	aiText      = 3
	aiActivity  = 4
	aiConfigure = 5

	aiDone          = 0
	aiNeedsApproval = 1
	aiFailed        = 2
	aiNotFound      = 3
)

const (
	stateDone          = "done"
	stateNeedsApproval = "needs-approval"
	stateFailed        = "failed"
)

// maxQuestion: an ASK's data must fit one IPC message.
const maxQuestion = 240

// maxText bounds an AI answer read back.
const maxText = 16 << 10

// maxApps bounds a listing.
const maxApps = 256

var errNoCore = &apiError{503, "not paired"}

type oceansSystem struct {
	sysinfo oceans.Handle
	ai      oceans.Handle
	// The paired Core capability (0 while unpaired), set by main.
	core func() oceans.Handle
}

func (s *oceansSystem) Info() (SystemInfo, error) {
	if s.sysinfo == 0 {
		return SystemInfo{}, &apiError{503, "the bridge has no system information"}
	}
	var info SystemInfo
	var err error
	read := func(kind uint64) []byte {
		if err != nil {
			return nil
		}
		var data []byte
		data, err = oceans.SystemInfo(s.sysinfo, kind)
		return data
	}
	kernel, memory, uptime, processes := read(sysinfoKernel), read(sysinfoMemory), read(sysinfoUptime), read(sysinfoProcesses)
	if err != nil {
		return SystemInfo{}, err
	}
	info.Kernel = parseKernel(kernel)
	info.Memory = parseMemory(memory)
	info.UptimeSeconds = parseUptime(uptime)
	info.Processes = parseProcesses(processes)
	return info, nil
}

func parseKernel(data []byte) Kernel {
	if len(data) < 40 {
		return Kernel{}
	}
	return Kernel{ABI: binary.LittleEndian.Uint64(data), Version: cText(data[8:24]), Arch: cText(data[24:40])}
}

func parseMemory(data []byte) Memory {
	if len(data) < 32 {
		return Memory{}
	}
	page := binary.LittleEndian.Uint64(data)
	return Memory{
		TotalMiB:      binary.LittleEndian.Uint64(data[8:]) * page >> 20,
		FreeMiB:       binary.LittleEndian.Uint64(data[16:]) * page >> 20,
		KernelHeapKiB: binary.LittleEndian.Uint64(data[24:]) >> 10,
	}
}

func parseUptime(data []byte) uint64 {
	if len(data) < 16 {
		return 0
	}
	ticks, hz := binary.LittleEndian.Uint64(data), binary.LittleEndian.Uint64(data[8:])
	if hz == 0 {
		return 0
	}
	return ticks / hz
}

// parseProcesses: the running ones, largest first.
func parseProcesses(data []byte) []Process {
	running := []Process{}
	for ; len(data) >= 64; data = data[64:] {
		if int64(binary.LittleEndian.Uint64(data[16:])) != -1<<63 {
			continue // exited
		}
		running = append(running, Process{
			ID:        binary.LittleEndian.Uint64(data),
			Parent:    binary.LittleEndian.Uint64(data[8:]),
			MemoryKiB: binary.LittleEndian.Uint64(data[24:]) >> 10,
			Name:      cText(data[32:64]),
		})
	}
	sort.SliceStable(running, func(i, j int) bool { return running[i].MemoryKiB > running[j].MemoryKiB })
	return running
}

func cText(b []byte) string {
	if i := strings.IndexByte(string(b), 0); i >= 0 {
		b = b[:i]
	}
	return strings.ToValidUTF8(string(b), "?")
}

// call makes one Core request through the paired capability.
func (s *oceansSystem) call(op uint64, data []byte) ([]byte, error) {
	core := s.core()
	if core == 0 {
		return nil, errNoCore
	}
	reply, err := oceans.Call(core, op, data, nil)
	if err != nil {
		if err == oceans.ErrPeerClosed {
			return nil, &apiError{503, "Oceans Core is unavailable"}
		}
		return nil, err
	}
	for _, h := range reply.Handles {
		_ = oceans.Close(h)
	}
	if reply.Label != 0 {
		if e, ok := coreErrors[reply.Label]; ok {
			return nil, e
		}
		return nil, &apiError{502, "Oceans Core failed the request"}
	}
	return reply.Data, nil
}

func (s *oceansSystem) field(id string, which byte) string {
	data, err := s.call(coreInfo, append([]byte{which}, id...))
	if err != nil {
		return ""
	}
	return strings.ToValidUTF8(string(data), "?")
}

func (s *oceansSystem) Apps() ([]App, error) {
	apps := []App{}
	for index := uint32(0); index < maxApps; index++ {
		data, err := s.call(coreList, binary.LittleEndian.AppendUint32(nil, index))
		var api *apiError
		if errors.As(err, &api) && api.status == 404 {
			break
		}
		if err != nil {
			return nil, err
		}
		app, ok := parseListEntry(data)
		if !ok {
			return nil, &apiError{502, "Oceans Core sent a malformed list"}
		}
		app.Kind = s.field(app.ID, fieldKind)
		app.Runtime = s.field(app.ID, fieldRuntime)
		app.Publisher = s.field(app.ID, fieldPublisher)
		apps = append(apps, app)
	}
	return apps, nil
}

// parseListEntry: `[running u8]` + `ID\0VERSION\0NAME`.
func parseListEntry(data []byte) (App, bool) {
	if len(data) < 2 {
		return App{}, false
	}
	fields := strings.SplitN(strings.ToValidUTF8(string(data[1:]), "?"), "\x00", 3)
	if len(fields) != 3 || !validAppID(fields[0]) {
		return App{}, false
	}
	return App{ID: fields[0], Version: fields[1], Name: fields[2], Running: data[0] != 0}, true
}

func (s *oceansSystem) Start(id, args string) error {
	data := append([]byte{runDetach, byte(len(id))}, id...)
	_, err := s.call(coreRun, append(data, args...))
	return err
}

func (s *oceansSystem) Stop(id string) error {
	_, err := s.call(coreStop, []byte(id))
	return err
}

func (s *oceansSystem) Permissions(id string) ([]Permission, error) {
	list := []Permission{}
	for index := 0; index < 64; index++ {
		data, err := s.call(corePermission, append([]byte{byte(index)}, id...))
		var api *apiError
		if errors.As(err, &api) && api.status == 404 {
			if index == 0 {
				// Not installed, or no permissions: tell which.
				if _, err := s.call(coreInfo, append([]byte{fieldVersion}, id...)); err != nil {
					return nil, err
				}
			}
			break
		}
		if err != nil {
			return nil, err
		}
		p, ok := parsePermission(data)
		if !ok {
			return nil, &apiError{502, "Oceans Core sent a malformed permission"}
		}
		list = append(list, p)
	}
	return list, nil
}

// parsePermission: `[permission u8][decision u8][reason]`.
func parsePermission(data []byte) (Permission, bool) {
	if len(data) < 2 || int(data[0]) >= len(permissions) || int(data[1]) >= len(decisions) {
		return Permission{}, false
	}
	p := permissions[data[0]]
	return Permission{
		Name:        p.name,
		Description: p.description,
		Decision:    decisions[data[1]],
		Reason:      strings.ToValidUTF8(string(data[2:]), "?"),
	}, true
}

func (s *oceansSystem) Audit() ([]string, error) {
	entries := []string{}
	for index := uint32(0); index < 256; index++ {
		data, err := s.call(coreAudit, binary.LittleEndian.AppendUint32(nil, index))
		var api *apiError
		if errors.As(err, &api) && api.status == 404 {
			break
		}
		if err != nil {
			return nil, err
		}
		entries = append(entries, strings.ToValidUTF8(string(data), "?"))
	}
	return entries, nil
}

func (s *oceansSystem) aiCall(op uint64, data []byte, handles []oceans.Handle) (oceans.Message, error) {
	if s.ai == 0 {
		for _, h := range handles {
			_ = oceans.Close(h)
		}
		return oceans.Message{}, &apiError{503, "this system has no AI runtime"}
	}
	reply, err := oceans.Call(s.ai, op, data, handles)
	if err != nil {
		if err == oceans.ErrPeerClosed {
			return reply, &apiError{503, "the AI runtime is unavailable"}
		}
		return reply, err
	}
	for _, h := range reply.Handles {
		_ = oceans.Close(h)
	}
	return reply, nil
}

// Ask starts an AI session with the authority the paired browser's user
// may delegate: a Core capability minted from the paired one, limited to
// querying and running apps (as the shell does, ADR-0051).
func (s *oceansSystem) Ask(question string) (AISession, error) {
	core := s.core()
	if core == 0 {
		return AISession{}, errNoCore
	}
	minted, err := oceans.Call(core, coreMint, []byte{accessQuery | accessRun}, nil)
	if err != nil || minted.Label != 0 || len(minted.Handles) != 1 {
		for _, h := range minted.Handles {
			_ = oceans.Close(h)
		}
		return AISession{}, &apiError{502, "cannot delegate to the AI session"}
	}
	reply, err := s.aiCall(aiAsk, []byte(question), minted.Handles)
	if err != nil {
		return AISession{}, err
	}
	return s.session(reply)
}

func (s *oceansSystem) Continue(session uint32, approve bool) (AISession, error) {
	data := binary.LittleEndian.AppendUint32(nil, session)
	if approve {
		data = append(data, 1)
	} else {
		data = append(data, 0)
	}
	reply, err := s.aiCall(aiContinue, data, nil)
	if err != nil {
		return AISession{}, err
	}
	if reply.Label == aiNotFound {
		return AISession{}, &apiError{404, "the AI session is gone"}
	}
	return s.session(reply)
}

// session reads an ASK or CONTINUE reply, fetching the rest of its text.
func (s *oceansSystem) session(reply oceans.Message) (AISession, error) {
	state, ok := map[uint64]string{aiDone: stateDone, aiNeedsApproval: stateNeedsApproval, aiFailed: stateFailed}[reply.Label]
	if !ok || len(reply.Data) < 8 {
		return AISession{}, &apiError{502, "the AI runtime refused the request"}
	}
	id := binary.LittleEndian.Uint32(reply.Data)
	total := int(binary.LittleEndian.Uint32(reply.Data[4:]))
	text := append([]byte(nil), reply.Data[8:]...)
	for len(text) < total && len(text) < maxText {
		request := binary.LittleEndian.AppendUint32(nil, id)
		request = binary.LittleEndian.AppendUint32(request, uint32(len(text)))
		more, err := s.aiCall(aiText, request, nil)
		if err != nil || more.Label != aiDone || len(more.Data) == 0 {
			break
		}
		text = append(text, more.Data...)
	}
	return AISession{Session: id, State: state, Text: strings.ToValidUTF8(string(text), "?")}, nil
}

// Activity: the AI's activity log, newest first.
func (s *oceansSystem) Activity() ([]string, error) {
	entries := []string{}
	for index := uint32(0); index < 64; index++ {
		reply, err := s.aiCall(aiActivity, binary.LittleEndian.AppendUint32(nil, index), nil)
		if err != nil {
			return nil, err
		}
		if reply.Label != aiDone {
			break
		}
		entries = append(entries, strings.ToValidUTF8(string(reply.Data), "?"))
	}
	return entries, nil
}

func (s *oceansSystem) SetModel(url, model string) (string, error) {
	reply, err := s.aiCall(aiConfigure, []byte(url+" "+model), nil)
	if err != nil {
		return "", err
	}
	note := strings.ToValidUTF8(string(reply.Data), "?")
	if reply.Label != aiDone {
		if note == "" {
			note = "the AI runtime refused the setting"
		}
		return "", &apiError{400, note}
	}
	return note, nil
}
