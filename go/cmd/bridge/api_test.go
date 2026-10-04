package main

import (
	"encoding/json"
	"strings"
	"testing"
)

const testToken = "0123456789abcdef0123456789abcdef"

// fake is the system as the routes see it, recording what they asked.
type fake struct {
	calls    []string
	started  map[string]string
	sessions uint32
}

func (f *fake) Info() (SystemInfo, error) {
	return SystemInfo{Memory: Memory{TotalMiB: 240, FreeMiB: 200}, UptimeSeconds: 5,
		Processes: []Process{{ID: 1, Name: "init", MemoryKiB: 64}}}, nil
}
func (f *fake) Apps() ([]App, error) {
	return []App{{ID: "app.oceans.hello", Version: "1.0.0", Name: "Hello", Kind: "app", Runtime: "native"}}, nil
}
func (f *fake) Start(id, args string) error {
	if id != "app.oceans.hello" {
		return coreErrors[1]
	}
	f.started[id] = args
	return nil
}
func (f *fake) Stop(id string) error {
	if _, ok := f.started[id]; !ok {
		return coreErrors[5]
	}
	delete(f.started, id)
	return nil
}
func (f *fake) Permissions(id string) ([]Permission, error) {
	p, _ := parsePermission([]byte{3, 3, 'w', 'h', 'y'})
	return []Permission{p}, nil
}
func (f *fake) Audit() ([]string, error) { return []string{"granted network to app.oceans.hello"}, nil }
func (f *fake) Ask(question string) (AISession, error) {
	f.sessions++
	f.calls = append(f.calls, "ask "+question)
	if strings.HasPrefix(question, "start") {
		return AISession{Session: f.sessions, State: stateNeedsApproval, Text: "start the app Hello (app.oceans.hello)"}, nil
	}
	return AISession{Session: f.sessions, State: stateDone, Text: "Memory: 200 MiB free"}, nil
}
func (f *fake) Continue(session uint32, approve bool) (AISession, error) {
	f.calls = append(f.calls, "continue")
	if approve {
		return AISession{Session: session, State: stateDone, Text: "Done"}, nil
	}
	return AISession{Session: session, State: stateDone, Text: "left it"}, nil
}
func (f *fake) Activity() ([]string, error) { return []string{"asked: hi"}, nil }
func (f *fake) SetModel(url, model string) (string, error) {
	f.calls = append(f.calls, "model "+url+" "+model)
	return "", nil
}

func newTestBridge(t *testing.T) (*bridge, *fake, *[]string) {
	t.Helper()
	sys := &fake{started: map[string]string{}}
	var logged []string
	b := newBridge(sys, &site{files: map[string]*asset{}}, func(s string) { logged = append(logged, s) })
	if err := b.pair(testToken); err != nil {
		t.Fatal(err)
	}
	return b, sys, &logged
}

// do serves a request, authorized with the token unless `auth` is "".
func do(b *bridge, method, path, auth, body string, extra ...string) *response {
	req := &request{method: method, path: path, header: map[string]string{"host": "oceans:8080"}, body: []byte(body)}
	if auth != "" {
		req.header["authorization"] = "Bearer " + auth
	}
	if method == "POST" {
		req.header["content-type"] = "application/json"
	}
	for i := 0; i+1 < len(extra); i += 2 {
		req.header[extra[i]] = extra[i+1]
	}
	return b.serve(req)
}

func header(resp *response, name string) string {
	value, _ := resp.get(name)
	return value
}

func TestEveryAPINeedsTheToken(t *testing.T) {
	b, _, _ := newTestBridge(t)
	for _, path := range []string{"/api/system", "/api/apps", "/api/apps/app.oceans.hello/permissions",
		"/api/audit", "/api/ai/activity", "/api/nothing"} {
		for _, auth := range []string{"", "0123456789abcdef0123456789abcdee", testToken + "0"} {
			resp := do(b, "GET", path, auth, "")
			if resp.status != 401 || header(resp, "WWW-Authenticate") == "" {
				t.Errorf("%s with %q: %d", path, auth, resp.status)
			}
		}
	}
	for _, path := range []string{"/api/apps/app.oceans.hello/start", "/api/ai/ask", "/api/ai/model"} {
		if resp := do(b, "POST", path, "", "{}"); resp.status != 401 {
			t.Errorf("POST %s: %d", path, resp.status)
		}
	}
}

func TestServesTheSystemWithTheToken(t *testing.T) {
	b, _, _ := newTestBridge(t)
	resp := do(b, "GET", "/api/system", testToken, "")
	if resp.status != 200 {
		t.Fatalf("status %d: %s", resp.status, resp.body)
	}
	var info SystemInfo
	if err := json.Unmarshal(resp.body, &info); err != nil || info.Memory.TotalMiB != 240 {
		t.Fatalf("%s (%v)", resp.body, err)
	}
	for name, want := range map[string]string{
		"Content-Type":            "application/json; charset=utf-8",
		"Cache-Control":           "no-store",
		"X-Content-Type-Options":  "nosniff",
		"X-Frame-Options":         "DENY",
		"Content-Security-Policy": "default-src 'none'; frame-ancestors 'none'",
	} {
		if got := header(resp, name); got != want {
			t.Errorf("%s: %q", name, got)
		}
	}
	if _, ok := resp.get("Access-Control-Allow-Origin"); ok {
		t.Error("CORS header")
	}
}

func TestTheCookieWorksLikeTheBearer(t *testing.T) {
	b, _, logged := newTestBridge(t)
	if resp := do(b, "POST", "/api/session", "", `{"token":"`+strings.Repeat("0", 32)+`"}`); resp.status != 401 {
		t.Fatalf("a wrong code: %d", resp.status)
	}
	if len(*logged) != 1 {
		t.Fatalf("refusal not logged: %q", *logged)
	}
	resp := do(b, "POST", "/api/session", "", `{"token":"`+testToken+`"}`)
	cookie := header(resp, "Set-Cookie")
	if resp.status != 204 || !strings.Contains(cookie, "HttpOnly") || !strings.Contains(cookie, "SameSite=Strict") {
		t.Fatalf("login: %d %q", resp.status, cookie)
	}
	value := strings.TrimPrefix(strings.Split(cookie, ";")[0], sessionCookie+"=")
	if resp := do(b, "GET", "/api/apps", "", "", "cookie", sessionCookie+"="+value); resp.status != 200 {
		t.Fatalf("with the cookie: %d", resp.status)
	}
	var state map[string]bool
	_ = json.Unmarshal(do(b, "GET", "/api/session", "", "", "cookie", sessionCookie+"="+value).body, &state)
	if !state["paired"] || !state["authenticated"] {
		t.Fatalf("session %v", state)
	}
	if resp := do(b, "DELETE", "/api/session", "", ""); !strings.Contains(header(resp, "Set-Cookie"), "Max-Age=0") {
		t.Fatal("logout keeps the cookie")
	}
}

func TestUnpairingRevokesTheToken(t *testing.T) {
	b, _, _ := newTestBridge(t)
	if !b.unpair() || b.unpair() {
		t.Fatal("unpair reports wrongly")
	}
	if resp := do(b, "GET", "/api/system", testToken, ""); resp.status != 401 {
		t.Fatalf("after unpairing: %d", resp.status)
	}
	if resp := do(b, "POST", "/api/session", "", `{"token":"`+testToken+`"}`); resp.status != 401 {
		t.Fatalf("login after unpairing: %d", resp.status)
	}
	// An empty token never matches an unpaired bridge.
	if b.authorized(&request{header: map[string]string{"authorization": "Bearer "}}) {
		t.Fatal("empty token accepted")
	}
	for _, bad := range []string{"", "short", strings.Repeat("g", 32), strings.Repeat("a", 129)} {
		if b.pair(bad) == nil {
			t.Errorf("paired with %q", bad)
		}
	}
}

func TestRefusesCrossSiteChanges(t *testing.T) {
	b, sys, _ := newTestBridge(t)
	start := "/api/apps/app.oceans.hello/start"
	for _, extra := range [][]string{
		{"origin", "http://evil.example"},
		{"sec-fetch-site", "cross-site"},
		{"sec-fetch-site", "same-site"},
	} {
		if resp := do(b, "POST", start, testToken, "{}", extra...); resp.status != 403 {
			t.Errorf("%v: %d", extra, resp.status)
		}
	}
	if resp := do(b, "POST", start, testToken, "{}", "content-type", "text/plain"); resp.status != 415 {
		t.Errorf("text/plain: %d", resp.status)
	}
	if len(sys.started) != 0 {
		t.Fatal("a refused request started an app")
	}
	resp := do(b, "POST", start, testToken, "{}", "origin", "http://oceans:8080", "sec-fetch-site", "same-origin")
	if resp.status != 200 {
		t.Fatalf("same origin: %d %s", resp.status, resp.body)
	}
}

func TestStartsAndStopsApps(t *testing.T) {
	b, sys, logged := newTestBridge(t)
	if resp := do(b, "POST", "/api/apps/app.oceans.hello/start", testToken, `{"args":"wait"}`); resp.status != 200 {
		t.Fatalf("start: %d %s", resp.status, resp.body)
	}
	if sys.started["app.oceans.hello"] != "wait" {
		t.Fatalf("started %v", sys.started)
	}
	if resp := do(b, "POST", "/api/apps/app.oceans.hello/stop", testToken, ""); resp.status != 200 {
		t.Fatalf("stop: %d %s", resp.status, resp.body)
	}
	if resp := do(b, "POST", "/api/apps/app.oceans.hello/stop", testToken, ""); resp.status != 409 {
		t.Fatalf("stop again: %d", resp.status)
	}
	if resp := do(b, "POST", "/api/apps/app.oceans.nope/start", testToken, ""); resp.status != 404 {
		t.Fatalf("not installed: %d", resp.status)
	}
	if len(*logged) != 2 {
		t.Fatalf("log %q", *logged)
	}
	for _, c := range []struct {
		method, path, body string
		status             int
	}{
		{"GET", "/api/apps/app.oceans.hello/start", "", 405},
		{"POST", "/api/apps/App.Hello/start", "{}", 400},
		{"POST", "/api/apps/app.oceans.hello/start", `{"args":"a\nb"}`, 400},
		{"POST", "/api/apps/app.oceans.hello/start", `{"args":"` + strings.Repeat("a", 161) + `"}`, 400},
		{"POST", "/api/apps/app.oceans.hello/start", `{"args":"x","more":1}`, 400},
		{"POST", "/api/apps/app.oceans.hello/start", `{"args":"x"}{}`, 400},
		{"POST", "/api/apps/app.oceans.hello/start", `["x"]`, 400},
		{"POST", "/api/apps/app.oceans.hello/start", `{"args":1}`, 400},
		{"POST", "/api/apps/app.oceans.hello/stop", `{"x":1}`, 400},
		{"POST", "/api/apps/app.oceans.hello/remove", "{}", 404},
		{"POST", "/api/system", "{}", 405},
	} {
		if resp := do(b, c.method, c.path, testToken, c.body); resp.status != c.status {
			t.Errorf("%s %s %s: %d, want %d", c.method, c.path, c.body, resp.status, c.status)
		}
	}
}

func TestListsAppsAndPermissions(t *testing.T) {
	b, _, _ := newTestBridge(t)
	resp := do(b, "GET", "/api/apps/app.oceans.hello/permissions", testToken, "")
	var list []Permission
	if err := json.Unmarshal(resp.body, &list); err != nil || len(list) != 1 {
		t.Fatalf("%s %v", resp.body, err)
	}
	if list[0].Name != "network" || list[0].Decision != "undecided" || list[0].Reason != "why" ||
		list[0].Description == "" {
		t.Fatalf("%+v", list[0])
	}
	if resp := do(b, "GET", "/api/apps", testToken, ""); !strings.Contains(string(resp.body), `"id":"app.oceans.hello"`) {
		t.Fatalf("%s", resp.body)
	}
}

func TestApprovalsComeFromThePairedBrowser(t *testing.T) {
	b, sys, logged := newTestBridge(t)
	var session AISession
	resp := do(b, "POST", "/api/ai/ask", testToken, `{"question":"start the hello app"}`)
	if err := json.Unmarshal(resp.body, &session); err != nil || session.State != stateNeedsApproval {
		t.Fatalf("%d %s", resp.status, resp.body)
	}
	// Only sessions this browser started, and each answered once.
	if resp := do(b, "POST", "/api/ai/continue", testToken, `{"session":99,"approve":true}`); resp.status != 404 {
		t.Fatalf("someone else's session: %d", resp.status)
	}
	if resp := do(b, "POST", "/api/ai/continue", testToken, `{"session":1}`); resp.status != 400 {
		t.Fatalf("no answer: %d", resp.status)
	}
	resp = do(b, "POST", "/api/ai/continue", testToken, `{"session":1,"approve":true}`)
	if resp.status != 200 || !strings.Contains(string(resp.body), "Done") {
		t.Fatalf("approve: %d %s", resp.status, resp.body)
	}
	if resp := do(b, "POST", "/api/ai/continue", testToken, `{"session":1,"approve":true}`); resp.status != 404 {
		t.Fatalf("answered twice: %d", resp.status)
	}
	if len(*logged) != 1 || !strings.Contains((*logged)[0], "approved by the user in the paired browser: start the app Hello") {
		t.Fatalf("log %q", *logged)
	}
	if strings.Join(sys.calls, ",") != "ask start the hello app,continue" {
		t.Fatalf("calls %q", sys.calls)
	}
	// A new pairing forgets the old browser's sessions.
	do(b, "POST", "/api/ai/ask", testToken, `{"question":"start it"}`)
	_ = b.pair(strings.Repeat("ab", 16))
	if resp := do(b, "POST", "/api/ai/continue", strings.Repeat("ab", 16), `{"session":2,"approve":true}`); resp.status != 404 {
		t.Fatalf("after re-pairing: %d", resp.status)
	}
	for _, body := range []string{`{"question":""}`, `{"question":"a\u0000b"}`, `{"question":"` + strings.Repeat("q", 241) + `"}`} {
		if resp := do(b, "POST", "/api/ai/ask", strings.Repeat("ab", 16), body); resp.status != 400 {
			t.Errorf("%s: %d", body, resp.status)
		}
	}
}

func TestSetsTheModel(t *testing.T) {
	b, sys, _ := newTestBridge(t)
	if resp := do(b, "POST", "/api/ai/model", testToken, `{"url":"http://10.0.2.2:11434/v1","model":"llama3.1"}`); resp.status != 200 {
		t.Fatalf("%d %s", resp.status, resp.body)
	}
	if sys.calls[0] != "model http://10.0.2.2:11434/v1 llama3.1" {
		t.Fatalf("%q", sys.calls)
	}
	if resp := do(b, "POST", "/api/ai/model", testToken, `{"url":"http://x y","model":"m"}`); resp.status != 400 {
		t.Fatalf("spaces: %d", resp.status)
	}
}
