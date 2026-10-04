// Package agent runs AI agent sessions (ADR-0051, ADR-0007).
//
// An agent acts only through tools. Each tool says whether it only reads
// (run without asking) or changes the system (run only after the user
// approves that very action). The approval text comes from the tool, in
// the system's words, never from the model. Every tool call, its
// arguments, the decision and the result go to the activity log.
//
// A session holds the capabilities its requester delegated (Env); a tool
// can use nothing else.
package agent

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"github.com/kaiserklowns-kkg/kernel/go/ai/model"
)

// Sensitivity of a tool (ADR-0007).
type Sensitivity int

const (
	// ReadOnly tools inspect non-sensitive state; they run without asking.
	ReadOnly Sensitivity = iota
	// Changes tools change the system; each call needs the user's approval.
	Changes
)

// Env is what a session may use: capabilities delegated by its requester.
type Env struct {
	// Values the tools understand (e.g. a Core capability); nil when not
	// delegated.
	Capabilities map[string]any
}

// Tool is one thing an agent can do.
type Tool interface {
	Spec() model.Function
	Sensitivity() Sensitivity
	// Describe checks the arguments and says, in the system's words,
	// what running the tool will do (shown when asking for approval).
	Describe(args map[string]any, env Env) (string, error)
	Run(args map[string]any, env Env) (string, error)
}

// State of a session.
type State int

const (
	// Working: between steps (never seen by a client).
	Working State = iota
	// AwaitingApproval: Session.Question says what for.
	AwaitingApproval
	// Done: Session.Answer is the final answer.
	Done
	// Failed: Session.Answer says why.
	Failed
)

// MaxSteps bounds model calls per session.
const MaxSteps = 8

// Session is one request to the agent.
type Session struct {
	ID       uint32
	State    State
	Answer   string
	Question string
	messages []model.Message
	env      Env
	queue    []model.ToolCall
	pending  *pending
	steps    int
}

type pending struct {
	call model.ToolCall
	tool Tool
	args map[string]any
}

// Runtime holds the model, the tools, the sessions and the activity log.
type Runtime struct {
	Model model.Model
	Tools []Tool
	// Prompt is the system prompt.
	Prompt string
	// Record receives every activity entry as it happens (e.g. the log).
	Record func(string)

	sessions map[uint32]*Session
	next     uint32
	activity []string
}

// MaxSessions open at once; the oldest is dropped beyond that.
const MaxSessions = 8

// MaxActivity entries kept (newest first).
const MaxActivity = 64

func (r *Runtime) record(format string, args ...any) {
	entry := fmt.Sprintf(format, args...)
	r.activity = append([]string{entry}, r.activity...)
	if len(r.activity) > MaxActivity {
		r.activity = r.activity[:MaxActivity]
	}
	if r.Record != nil {
		r.Record(entry)
	}
}

// Activity entry `index`, newest first.
func (r *Runtime) Activity(index int) (string, bool) {
	if index < 0 || index >= len(r.activity) {
		return "", false
	}
	return r.activity[index], true
}

func (r *Runtime) tool(name string) Tool {
	for _, t := range r.Tools {
		if t.Spec().Name == name {
			return t
		}
	}
	return nil
}

func (r *Runtime) specs() []model.Tool {
	specs := make([]model.Tool, 0, len(r.Tools))
	for _, t := range r.Tools {
		specs = append(specs, model.Tool{Type: "function", Function: t.Spec()})
	}
	return specs
}

// Ask starts a session and runs it until it is done, failed, or waiting
// for an approval.
func (r *Runtime) Ask(prompt string, env Env) *Session {
	if r.sessions == nil {
		r.sessions = map[uint32]*Session{}
	}
	if len(r.sessions) >= MaxSessions {
		oldest := ^uint32(0)
		for id := range r.sessions {
			oldest = min(oldest, id)
		}
		delete(r.sessions, oldest)
	}
	r.next++
	s := &Session{ID: r.next, env: env}
	s.messages = []model.Message{
		{Role: "system", Content: r.Prompt},
		{Role: "user", Content: prompt},
	}
	r.sessions[s.ID] = s
	r.record("session %d: asked %q", s.ID, prompt)
	r.advance(s)
	return s
}

// ErrNoSession: unknown session, or not waiting for an approval.
var ErrNoSession = errors.New("no session waiting for an approval")

// Continue answers a session's approval question and runs it on.
func (r *Runtime) Continue(id uint32, approve bool) (*Session, error) {
	s, ok := r.sessions[id]
	if !ok || s.State != AwaitingApproval || s.pending == nil {
		return nil, ErrNoSession
	}
	p := s.pending
	s.pending = nil
	s.Question = ""
	s.State = Working
	var result string
	if approve {
		out, err := p.tool.Run(p.args, s.env)
		result = outcome(out, err)
		r.record("session %d: %s %s approved by the user: %s", s.ID, p.call.Function.Name, compact(p.call.Function.Arguments), result)
	} else {
		result = "The user denied this action. Do not try it again; tell the user it was not done."
		r.record("session %d: %s %s denied by the user", s.ID, p.call.Function.Name, compact(p.call.Function.Arguments))
	}
	s.messages = append(s.messages, model.Message{Role: "tool", Content: result, ToolCallID: p.call.ID})
	r.advance(s)
	return s, nil
}

// Session returns a session by id.
func (r *Runtime) Session(id uint32) (*Session, bool) {
	s, ok := r.sessions[id]
	return s, ok
}

// End forgets a session (and so drops its capabilities).
func (r *Runtime) End(id uint32) {
	delete(r.sessions, id)
}

func outcome(out string, err error) string {
	if err != nil {
		return "error: " + err.Error()
	}
	return out
}

func compact(arguments string) string {
	arguments = strings.TrimSpace(arguments)
	if arguments == "" {
		return "{}"
	}
	return arguments
}

// advance runs tool calls and model steps until the session finishes or
// needs an approval.
func (r *Runtime) advance(s *Session) {
	for {
		for len(s.queue) > 0 {
			call := s.queue[0]
			s.queue = s.queue[1:]
			if r.runCall(s, call) {
				return // waiting for an approval
			}
		}
		if s.steps >= MaxSteps {
			s.State, s.Answer = Failed, fmt.Sprintf("stopped after %d steps without an answer", MaxSteps)
			r.record("session %d: failed: %s", s.ID, s.Answer)
			return
		}
		s.steps++
		reply, err := r.Model.Complete(s.messages, r.specs())
		if err != nil {
			s.State, s.Answer = Failed, "model: "+err.Error()
			r.record("session %d: failed: %s", s.ID, s.Answer)
			return
		}
		reply.Role = "assistant"
		s.messages = append(s.messages, reply)
		if len(reply.ToolCalls) == 0 {
			s.State, s.Answer = Done, strings.TrimSpace(reply.Content)
			r.record("session %d: answered", s.ID)
			return
		}
		s.queue = append(s.queue, reply.ToolCalls...)
	}
}

// runCall runs one tool call; true if the session now waits for approval.
func (r *Runtime) runCall(s *Session, call model.ToolCall) bool {
	reply := func(result string) {
		s.messages = append(s.messages, model.Message{Role: "tool", Content: result, ToolCallID: call.ID})
	}
	tool := r.tool(call.Function.Name)
	if tool == nil {
		r.record("session %d: unknown tool %q refused", s.ID, call.Function.Name)
		reply("error: there is no tool named " + call.Function.Name)
		return false
	}
	args := map[string]any{}
	if text := strings.TrimSpace(call.Function.Arguments); text != "" {
		if err := json.Unmarshal([]byte(text), &args); err != nil {
			r.record("session %d: %s refused: arguments are not a JSON object", s.ID, call.Function.Name)
			reply("error: the arguments must be a JSON object")
			return false
		}
	}
	description, err := tool.Describe(args, s.env)
	if err != nil {
		r.record("session %d: %s %s refused: %v", s.ID, call.Function.Name, compact(call.Function.Arguments), err)
		reply("error: " + err.Error())
		return false
	}
	if tool.Sensitivity() == ReadOnly {
		out, err := tool.Run(args, s.env)
		result := outcome(out, err)
		r.record("session %d: %s %s (read-only): %s", s.ID, call.Function.Name, compact(call.Function.Arguments), firstLine(result))
		reply(result)
		return false
	}
	s.pending = &pending{call: call, tool: tool, args: args}
	s.State, s.Question = AwaitingApproval, description
	r.record("session %d: %s %s needs approval: %s", s.ID, call.Function.Name, compact(call.Function.Arguments), description)
	return true
}

func firstLine(text string) string {
	if i := strings.IndexByte(text, '\n'); i >= 0 {
		return text[:i] + " ..."
	}
	return text
}
