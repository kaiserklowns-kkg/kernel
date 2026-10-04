// ai: the Oceans AI runtime (ADR-0051), a Go service run by the Go host.
//
// It serves the `ai` endpoint (the protocol is in protocol.go). Its own
// authority is small: a log, the network (for the model gateway),
// read-only system information and the system's trusted root
// certificates (gateway.go, ADR-0054). Everything else an agent may touch comes
// with each request: the requester delegates capabilities to the session
// (today an Oceans Core capability limited to querying and running apps,
// ADR-0048), and they are closed when the session ends.
//
// Sensitive actions wait for the requester to answer an approval question
// whose words come from the tool, not the model (ADR-0007). Every step is
// recorded in the activity log, which `ACTIVITY` returns and the system
// log keeps.
package main

import (
	"encoding/binary"
	"strings"

	"github.com/kaiserklowns-kkg/kernel/go/ai/agent"
	"github.com/kaiserklowns-kkg/kernel/go/ai/model"
	"github.com/kaiserklowns-kkg/kernel/go/ai/tools"
	"github.com/kaiserklowns-kkg/kernel/go/oceans"
)

const systemPrompt = "You are Oceans AI, the assistant built into the Oceans operating system. " +
	"Use the tools to look at the system or to act on it. The system asks the user before any " +
	"action that changes something; if the user says no, accept it and say it was not done. " +
	"Answer briefly, in plain words."

// unconfigured fails every session until `CONFIGURE`.
type unconfigured struct{}

func (unconfigured) Complete([]model.Message, []model.Tool) (model.Message, error) {
	return model.Message{}, errNotConfigured
}

type service struct {
	log     oceans.Handle
	net     oceans.Handle
	sysinfo oceans.Handle
	runtime *agent.Runtime
	// Delegated capabilities of open sessions, closed when they end.
	delegated map[uint32][]oceans.Handle
	// Final texts of ended sessions, for TEXT.
	texts map[uint32]string
	order []uint32
}

func (s *service) say(text string) {
	if s.log != 0 {
		_ = oceans.DebugWrite(s.log, "ai: "+text)
	}
}

func main() {
	server, ok := oceans.Find("provide", "ai")
	if !ok {
		return
	}
	s := &service{delegated: map[uint32][]oceans.Handle{}, texts: map[uint32]string{}}
	s.log, _ = oceans.Find("log", "log")
	s.net, _ = oceans.Find("use", "net")
	s.sysinfo, _ = oceans.Find("sysinfo", "sysinfo")
	s.runtime = &agent.Runtime{
		Model:  unconfigured{},
		Tools:  tools.All(),
		Prompt: systemPrompt,
		Record: func(entry string) { s.say("activity: " + entry) },
	}
	s.say("ready; " + itoa(len(s.runtime.Tools)) + " tools; no model configured yet")
	for {
		msg, err := oceans.Receive(server)
		if err == oceans.ErrPeerClosed {
			return // every client end is gone (init keeps one): shutdown
		}
		if err != nil {
			s.say("receive failed: " + err.Error())
			return
		}
		if msg.Closed || msg.Signals != 0 {
			continue
		}
		label, data, handles := s.handle(msg)
		if err := oceans.Reply(label, data, handles); err != nil {
			for _, h := range handles {
				_ = oceans.Close(h)
			}
		}
	}
}

func (s *service) handle(msg oceans.Message) (uint64, []byte, []oceans.Handle) {
	closeAll := func() {
		for _, h := range msg.Handles {
			_ = oceans.Close(h)
		}
	}
	switch msg.Label {
	case opAsk:
		prompt := strings.TrimSpace(string(msg.Data))
		if prompt == "" {
			closeAll()
			return statusBadRequest, nil, nil
		}
		env := agent.Env{Capabilities: map[string]any{"sysinfo": s.sysinfo}}
		// What the requester delegated: [core].
		if len(msg.Handles) >= 1 {
			env.Capabilities["core"] = msg.Handles[0]
		}
		for _, extra := range msg.Handles[min(len(msg.Handles), 1):] {
			_ = oceans.Close(extra)
		}
		session := s.runtime.Ask(prompt, env)
		s.delegated[session.ID] = msg.Handles[:min(len(msg.Handles), 1)]
		return s.reply(session)
	case opContinue:
		closeAll()
		if len(msg.Data) != 5 {
			return statusBadRequest, nil, nil
		}
		id := binary.LittleEndian.Uint32(msg.Data)
		session, err := s.runtime.Continue(id, msg.Data[4] != 0)
		if err != nil {
			return statusNotFound, nil, nil
		}
		return s.reply(session)
	case opText:
		closeAll()
		if len(msg.Data) != 8 {
			return statusBadRequest, nil, nil
		}
		text, ok := s.texts[binary.LittleEndian.Uint32(msg.Data)]
		offset := int(binary.LittleEndian.Uint32(msg.Data[4:]))
		if !ok || offset > len(text) {
			return statusNotFound, nil, nil
		}
		return statusDone, []byte(text[offset:min(len(text), offset+maxChunk)]), nil
	case opActivity:
		closeAll()
		if len(msg.Data) != 4 {
			return statusBadRequest, nil, nil
		}
		entry, ok := s.runtime.Activity(int(binary.LittleEndian.Uint32(msg.Data)))
		if !ok {
			return statusNotFound, nil, nil
		}
		return statusDone, []byte(entry[:min(len(entry), maxChunk)]), nil
	case opConfigure:
		defer closeAll() // the CA, read during configure
		return s.configure(string(msg.Data), msg.Handles)
	}
	closeAll()
	return statusBadRequest, nil, nil
}

// reply reports a session's state; an ended session's capabilities are
// closed and its text kept for TEXT.
func (s *service) reply(session *agent.Session) (uint64, []byte, []oceans.Handle) {
	var status uint64
	var text string
	switch session.State {
	case agent.AwaitingApproval:
		status, text = statusNeedsApproval, session.Question
	case agent.Done:
		status, text = statusDone, session.Answer
	default:
		status, text = statusFailed, session.Answer
	}
	s.keep(session.ID, text)
	if status != statusNeedsApproval {
		for _, h := range s.delegated[session.ID] {
			_ = oceans.Close(h)
		}
		delete(s.delegated, session.ID)
		s.runtime.End(session.ID)
	}
	data := binary.LittleEndian.AppendUint32(nil, session.ID)
	data = binary.LittleEndian.AppendUint32(data, uint32(len(text)))
	data = append(data, text[:min(len(text), maxChunk-8)]...)
	return status, data, nil
}

func (s *service) keep(id uint32, text string) {
	if _, ok := s.texts[id]; !ok {
		s.order = append(s.order, id)
	}
	s.texts[id] = text
	for len(s.order) > 16 {
		delete(s.texts, s.order[0])
		s.order = s.order[1:]
	}
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	var digits []byte
	for ; n > 0; n /= 10 {
		digits = append([]byte{byte('0' + n%10)}, digits...)
	}
	return string(digits)
}
