// Package tools are the AI runtime's tools on Oceans (ADR-0051). Each
// uses only a capability in the session's Env: "sysinfo" (read-only
// system information, the AI service's own) or "core" (an Oceans Core
// capability the requester minted for the session, ADR-0048).
package tools

import (
	"encoding/binary"
	"errors"
	"fmt"
	"strings"

	"github.com/kaiserklowns-kkg/kernel/go/ai/agent"
	"github.com/kaiserklowns-kkg/kernel/go/ai/model"
	"github.com/kaiserklowns-kkg/kernel/go/oceans"
)

// All returns the tools.
func All() []agent.Tool {
	return []agent.Tool{memory{}, processes{}, appsList{}, appsStart{}, appsStop{}, filesList{}, filesRead{}}
}

var noArguments = []byte(`{"type":"object","properties":{}}`)

func handle(env agent.Env, name string) (oceans.Handle, error) {
	h, ok := env.Capabilities[name].(oceans.Handle)
	if !ok || h == 0 {
		return 0, fmt.Errorf("this session was not given the %s capability", name)
	}
	return h, nil
}

// ---- System information (read-only, ADR-0020) ------------------------------

const (
	sysinfoMemory    = 1
	sysinfoProcesses = 3
)

type memory struct{}

func (memory) Spec() model.Function {
	return model.Function{
		Name:        "system_memory",
		Description: "Free and total memory of this computer, in MiB.",
		Parameters:  noArguments,
	}
}
func (memory) Sensitivity() agent.Sensitivity { return agent.ReadOnly }
func (memory) Describe(map[string]any, agent.Env) (string, error) {
	return "read the memory figures", nil
}
func (memory) Run(_ map[string]any, env agent.Env) (string, error) {
	sysinfo, err := handle(env, "sysinfo")
	if err != nil {
		return "", err
	}
	data, err := oceans.SystemInfo(sysinfo, sysinfoMemory)
	if err != nil {
		return "", err
	}
	if len(data) < 32 {
		return "", errors.New("short memory record")
	}
	page := binary.LittleEndian.Uint64(data[0:])
	total := binary.LittleEndian.Uint64(data[8:]) * page >> 20
	free := binary.LittleEndian.Uint64(data[16:]) * page >> 20
	return fmt.Sprintf("%d MiB free of %d MiB", free, total), nil
}

type processes struct{}

func (processes) Spec() model.Function {
	return model.Function{
		Name:        "system_processes",
		Description: "The running processes: name and memory used (KiB), largest first.",
		Parameters:  noArguments,
	}
}
func (processes) Sensitivity() agent.Sensitivity { return agent.ReadOnly }
func (processes) Describe(map[string]any, agent.Env) (string, error) {
	return "list the running processes", nil
}
func (processes) Run(_ map[string]any, env agent.Env) (string, error) {
	sysinfo, err := handle(env, "sysinfo")
	if err != nil {
		return "", err
	}
	data, err := oceans.SystemInfo(sysinfo, sysinfoProcesses)
	if err != nil {
		return "", err
	}
	type proc struct {
		name   string
		memory uint64
	}
	var running []proc
	for ; len(data) >= 64; data = data[64:] {
		if int64(binary.LittleEndian.Uint64(data[16:])) != -1<<63 {
			continue // exited
		}
		name := string(data[32:64])
		if i := strings.IndexByte(name, 0); i >= 0 {
			name = name[:i]
		}
		running = append(running, proc{name, binary.LittleEndian.Uint64(data[24:]) >> 10})
	}
	for i := 1; i < len(running); i++ {
		for j := i; j > 0 && running[j].memory > running[j-1].memory; j-- {
			running[j], running[j-1] = running[j-1], running[j]
		}
	}
	var out strings.Builder
	for i, p := range running {
		if i == 20 {
			fmt.Fprintf(&out, "... and %d more\n", len(running)-20)
			break
		}
		fmt.Fprintf(&out, "%s %d KiB\n", p.name, p.memory)
	}
	return strings.TrimSpace(out.String()), nil
}

// ---- Apps, through the session's Core capability (ADR-0045, ADR-0048) ------

const (
	coreList  = 2
	coreInfo  = 3
	coreRun   = 5
	coreStop  = 6
	fieldName = 0
	detach    = 1
)

var coreStatus = map[uint64]string{
	1: "not installed", 2: "bad request", 3: "package refused", 4: "the user has not yet decided its permissions (they can run it themselves first)",
	5: "not running", 9: "already running", 10: "storage failed", 11: "cannot be started", 12: "not allowed for this session",
}

func coreCall(core oceans.Handle, op uint64, data []byte) ([]byte, error) {
	reply, err := oceans.Call(core, op, data, nil)
	if err != nil {
		return nil, err
	}
	for _, h := range reply.Handles {
		_ = oceans.Close(h)
	}
	if reply.Label != 0 {
		if text, ok := coreStatus[reply.Label]; ok {
			return nil, errors.New(text)
		}
		return nil, fmt.Errorf("core status %d", reply.Label)
	}
	return reply.Data, nil
}

type appsList struct{}

func (appsList) Spec() model.Function {
	return model.Function{
		Name:        "apps_list",
		Description: "The installed apps: id, version, name, and whether each is running.",
		Parameters:  noArguments,
	}
}
func (appsList) Sensitivity() agent.Sensitivity { return agent.ReadOnly }
func (appsList) Describe(map[string]any, agent.Env) (string, error) {
	return "list the installed apps", nil
}
func (appsList) Run(_ map[string]any, env agent.Env) (string, error) {
	core, err := handle(env, "core")
	if err != nil {
		return "", err
	}
	var out strings.Builder
	for index := uint32(0); index < 256; index++ {
		data, err := coreCall(core, coreList, binary.LittleEndian.AppendUint32(nil, index))
		if err != nil {
			break
		}
		if len(data) < 1 {
			break
		}
		fields := strings.SplitN(string(data[1:]), "\x00", 3)
		for len(fields) < 3 {
			fields = append(fields, "?")
		}
		state := "installed"
		if data[0] != 0 {
			state = "running"
		}
		fmt.Fprintf(&out, "%s %s %q %s\n", fields[0], fields[1], fields[2], state)
	}
	if out.Len() == 0 {
		return "no apps are installed", nil
	}
	return strings.TrimSpace(out.String()), nil
}

// appID reads and checks the "id" argument and names the app (from Core,
// so the approval shows the installed app's real name).
func appID(args map[string]any, env agent.Env) (string, string, error) {
	id, _ := args["id"].(string)
	if id == "" || len(id) > 64 || strings.ContainsAny(id, " \x00/") {
		return "", "", errors.New(`"id" must be an app id such as app.oceans.hello`)
	}
	core, err := handle(env, "core")
	if err != nil {
		return "", "", err
	}
	name, err := coreCall(core, coreInfo, append([]byte{fieldName}, id...))
	if err != nil {
		return "", "", fmt.Errorf("%s: %w", id, err)
	}
	return id, string(name), nil
}

func appArgs(args map[string]any) (string, error) {
	text, _ := args["args"].(string)
	if len(text) > 160 || strings.ContainsAny(text, "\x00\r\n") {
		return "", errors.New(`"args" must be one short line`)
	}
	return strings.TrimSpace(text), nil
}

type appsStart struct{}

func (appsStart) Spec() model.Function {
	return model.Function{
		Name:        "apps_start",
		Description: "Start an installed app in the background. Needs the user's approval.",
		Parameters: []byte(`{"type":"object","properties":{` +
			`"id":{"type":"string","description":"the app id, from apps_list"},` +
			`"args":{"type":"string","description":"arguments for the app (optional)"}},"required":["id"]}`),
	}
}
func (appsStart) Sensitivity() agent.Sensitivity { return agent.Changes }
func (appsStart) Describe(args map[string]any, env agent.Env) (string, error) {
	id, name, err := appID(args, env)
	if err != nil {
		return "", err
	}
	extra, err := appArgs(args)
	if err != nil {
		return "", err
	}
	if extra != "" {
		return fmt.Sprintf("start the app %s (%s) with the arguments %q", name, id, extra), nil
	}
	return fmt.Sprintf("start the app %s (%s)", name, id), nil
}
func (appsStart) Run(args map[string]any, env agent.Env) (string, error) {
	id, name, err := appID(args, env)
	if err != nil {
		return "", err
	}
	extra, err := appArgs(args)
	if err != nil {
		return "", err
	}
	core, _ := handle(env, "core")
	data := append([]byte{detach, byte(len(id))}, id...)
	data = append(data, extra...)
	if _, err := coreCall(core, coreRun, data); err != nil {
		return "", err
	}
	return fmt.Sprintf("started %s (%s)", name, id), nil
}

type appsStop struct{}

func (appsStop) Spec() model.Function {
	return model.Function{
		Name:        "apps_stop",
		Description: "Stop a running app. Needs the user's approval.",
		Parameters: []byte(`{"type":"object","properties":{` +
			`"id":{"type":"string","description":"the app id, from apps_list"}},"required":["id"]}`),
	}
}
func (appsStop) Sensitivity() agent.Sensitivity { return agent.Changes }
func (appsStop) Describe(args map[string]any, env agent.Env) (string, error) {
	id, name, err := appID(args, env)
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("stop the app %s (%s)", name, id), nil
}
func (appsStop) Run(args map[string]any, env agent.Env) (string, error) {
	id, name, err := appID(args, env)
	if err != nil {
		return "", err
	}
	core, _ := handle(env, "core")
	if _, err := coreCall(core, coreStop, []byte(id)); err != nil {
		return "", err
	}
	return fmt.Sprintf("stopped %s (%s)", name, id), nil
}
