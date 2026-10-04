package agent

import (
	"errors"
	"strings"
	"testing"

	"github.com/kaiserklowns-kkg/kernel/go/ai/model"
)

// scripted answers each Complete with the next message, and records what
// it was sent.
type scripted struct {
	replies []model.Message
	seen    [][]model.Message
}

func (m *scripted) Complete(messages []model.Message, tools []model.Tool) (model.Message, error) {
	m.seen = append(m.seen, append([]model.Message(nil), messages...))
	if len(m.replies) == 0 {
		return model.Message{}, errors.New("no more replies")
	}
	next := m.replies[0]
	m.replies = m.replies[1:]
	return next, nil
}

func call(id, name, args string) model.ToolCall {
	return model.ToolCall{ID: id, Type: "function", Function: model.FunctionCall{Name: name, Arguments: args}}
}

type fakeTool struct {
	name        string
	sensitivity Sensitivity
	ran         []map[string]any
}

func (t *fakeTool) Spec() model.Function {
	return model.Function{Name: t.name, Description: "test", Parameters: []byte(`{"type":"object"}`)}
}
func (t *fakeTool) Sensitivity() Sensitivity { return t.sensitivity }
func (t *fakeTool) Describe(args map[string]any, env Env) (string, error) {
	if args["bad"] != nil {
		return "", errors.New("bad argument")
	}
	return "do " + t.name, nil
}
func (t *fakeTool) Run(args map[string]any, env Env) (string, error) {
	t.ran = append(t.ran, args)
	return t.name + " done", nil
}

func TestReadOnlyToolsRunWithoutAsking(t *testing.T) {
	look := &fakeTool{name: "look", sensitivity: ReadOnly}
	m := &scripted{replies: []model.Message{
		{ToolCalls: []model.ToolCall{call("1", "look", `{}`)}},
		{Content: "all fine"},
	}}
	r := &Runtime{Model: m, Tools: []Tool{look}, Prompt: "system"}
	s := r.Ask("how is it?", Env{})
	if s.State != Done || s.Answer != "all fine" {
		t.Fatalf("state %v answer %q", s.State, s.Answer)
	}
	if len(look.ran) != 1 {
		t.Fatalf("look ran %d times", len(look.ran))
	}
	last := m.seen[1][len(m.seen[1])-1]
	if last.Role != "tool" || last.Content != "look done" || last.ToolCallID != "1" {
		t.Fatalf("tool result sent as %+v", last)
	}
	if entry, _ := r.Activity(1); !strings.Contains(entry, "look {} (read-only): look done") {
		t.Fatalf("activity: %q", entry)
	}
}

func TestChangesWaitForApproval(t *testing.T) {
	act := &fakeTool{name: "act", sensitivity: Changes}
	m := &scripted{replies: []model.Message{
		{ToolCalls: []model.ToolCall{call("a", "act", `{"x":1}`)}},
		{Content: "done it"},
	}}
	r := &Runtime{Model: m, Tools: []Tool{act}}
	s := r.Ask("do it", Env{})
	if s.State != AwaitingApproval || s.Question != "do act" || len(act.ran) != 0 {
		t.Fatalf("state %v question %q ran %d", s.State, s.Question, len(act.ran))
	}
	s, err := r.Continue(s.ID, true)
	if err != nil || s.State != Done || s.Answer != "done it" || len(act.ran) != 1 {
		t.Fatalf("after approval: %v %v %q ran %d", err, s.State, s.Answer, len(act.ran))
	}
	if _, err := r.Continue(s.ID, true); !errors.Is(err, ErrNoSession) {
		t.Fatalf("approving twice: %v", err)
	}
}

func TestDeniedActionsDoNotRun(t *testing.T) {
	act := &fakeTool{name: "act", sensitivity: Changes}
	m := &scripted{replies: []model.Message{
		{ToolCalls: []model.ToolCall{call("a", "act", `{}`)}},
		{Content: "not done"},
	}}
	r := &Runtime{Model: m, Tools: []Tool{act}}
	s := r.Ask("do it", Env{})
	s, _ = r.Continue(s.ID, false)
	if s.State != Done || len(act.ran) != 0 {
		t.Fatalf("denied action ran: %v %d", s.State, len(act.ran))
	}
	last := m.seen[1][len(m.seen[1])-1]
	if !strings.Contains(last.Content, "denied") {
		t.Fatalf("model not told: %q", last.Content)
	}
	if entry, _ := r.Activity(1); !strings.Contains(entry, "act {} denied by the user") {
		t.Fatalf("activity: %q", entry)
	}
}

func TestBadCallsAreRefusedNotRun(t *testing.T) {
	act := &fakeTool{name: "act", sensitivity: Changes}
	m := &scripted{replies: []model.Message{
		{ToolCalls: []model.ToolCall{
			call("1", "nope", `{}`),
			call("2", "act", `not json`),
			call("3", "act", `{"bad":true}`),
		}},
		{Content: "gave up"},
	}}
	r := &Runtime{Model: m, Tools: []Tool{act}}
	s := r.Ask("x", Env{})
	if s.State != Done || len(act.ran) != 0 {
		t.Fatalf("state %v ran %d", s.State, len(act.ran))
	}
	results := m.seen[1][3:]
	for i, want := range []string{"no tool named nope", "JSON object", "bad argument"} {
		if !strings.Contains(results[i].Content, want) {
			t.Fatalf("result %d: %q lacks %q", i, results[i].Content, want)
		}
	}
}

func TestStepsAreBounded(t *testing.T) {
	look := &fakeTool{name: "look"}
	var replies []model.Message
	for range MaxSteps + 2 {
		replies = append(replies, model.Message{ToolCalls: []model.ToolCall{call("1", "look", "")}})
	}
	r := &Runtime{Model: &scripted{replies: replies}, Tools: []Tool{look}}
	if s := r.Ask("loop", Env{}); s.State != Failed || !strings.Contains(s.Answer, "steps") {
		t.Fatalf("state %v answer %q", s.State, s.Answer)
	}
}

func TestModelErrorsFailTheSession(t *testing.T) {
	r := &Runtime{Model: &scripted{}}
	if s := r.Ask("x", Env{}); s.State != Failed || !strings.HasPrefix(s.Answer, "model:") {
		t.Fatalf("state %v answer %q", s.State, s.Answer)
	}
}

func TestOldSessionsAreDropped(t *testing.T) {
	var replies []model.Message
	for range MaxSessions + 1 {
		replies = append(replies, model.Message{ToolCalls: []model.ToolCall{call("a", "act", "")}})
	}
	r := &Runtime{Model: &scripted{replies: replies}, Tools: []Tool{&fakeTool{name: "act", sensitivity: Changes}}}
	first := r.Ask("1", Env{})
	for range MaxSessions {
		r.Ask("more", Env{})
	}
	if _, ok := r.Session(first.ID); ok {
		t.Fatal("the oldest session was kept")
	}
}
