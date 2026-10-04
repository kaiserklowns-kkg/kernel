package model

import (
	"encoding/json"
	"strings"
	"testing"
)

type fakePoster struct {
	status int
	reply  string
	sent   []byte
	path   string
}

func (p *fakePoster) Post(path string, body []byte) (int, []byte, error) {
	p.path, p.sent = path, body
	return p.status, []byte(p.reply), nil
}

func TestCompleteSendsToolsAndReadsToolCalls(t *testing.T) {
	poster := &fakePoster{status: 200, reply: `{"choices":[{"message":{"role":"assistant","content":"",
		"tool_calls":[{"id":"c1","type":"function","function":{"name":"system_memory","arguments":"{}"}}]}}]}`}
	chat := Chat{Transport: poster, Path: "/v1/chat/completions", Model: "m"}
	tools := []Tool{{Type: "function", Function: Function{Name: "system_memory", Description: "d", Parameters: json.RawMessage(`{"type":"object"}`)}}}
	reply, err := chat.Complete([]Message{{Role: "user", Content: "hi"}}, tools)
	if err != nil {
		t.Fatal(err)
	}
	if len(reply.ToolCalls) != 1 || reply.ToolCalls[0].Function.Name != "system_memory" {
		t.Fatalf("reply %+v", reply)
	}
	var sent map[string]any
	if err := json.Unmarshal(poster.sent, &sent); err != nil {
		t.Fatal(err)
	}
	if sent["model"] != "m" || sent["stream"] != false || poster.path != "/v1/chat/completions" {
		t.Fatalf("sent %s to %s", poster.sent, poster.path)
	}
	if !strings.Contains(string(poster.sent), `"name":"system_memory"`) {
		t.Fatalf("tools not sent: %s", poster.sent)
	}
}

func TestCompleteErrors(t *testing.T) {
	for _, c := range []struct {
		status int
		reply  string
		want   string
	}{
		{500, `not json`, "HTTP 500"},
		{401, `{"error":{"message":"bad key"}}`, "bad key"},
		{200, `{"choices":[]}`, "no message"},
		{200, `garbage`, "unreadable"},
	} {
		_, err := Chat{Transport: &fakePoster{status: c.status, reply: c.reply}}.Complete(nil, nil)
		if err == nil || !strings.Contains(err.Error(), c.want) {
			t.Errorf("%d %q: %v, want %q", c.status, c.reply, err, c.want)
		}
	}
}
