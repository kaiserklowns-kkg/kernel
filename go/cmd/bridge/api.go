package main

import (
	"bytes"
	"encoding/json"
	"errors"
	"strconv"
	"strings"
)

// The System API as JSON (ADR-0058). Every route but /api/session needs
// the pairing token: as `Authorization: Bearer TOKEN`, or the HttpOnly
// session cookie the login sets. What the routes may do is what the
// paired capability allows (query, run, audit), never more: the bridge
// itself holds no authority over apps.

// sessionCookie carries the token for the browser; HttpOnly keeps it from
// page scripts, SameSite=Strict from other sites' requests.
const sessionCookie = "oceans_session"

// system is what the routes reach: the Oceans side (system.go), or a fake
// in tests.
type system interface {
	Info() (SystemInfo, error)
	Apps() ([]App, error)
	Start(id, args string) error
	Stop(id string) error
	Permissions(id string) ([]Permission, error)
	Audit() ([]string, error)
	Ask(question string) (AISession, error)
	Continue(session uint32, approve bool) (AISession, error)
	Activity() ([]string, error)
	SetModel(url, model string) (string, error)
	// Propose hands a downloaded package to Core, to install once the
	// user confirms on the device (ADR-0061).
	Propose(pkg []byte) error
	// WebBundle is a web app's version and bundle, verified by Core
	// (ADR-0064).
	WebBundle(id string) (string, []byte, error)
}

type SystemInfo struct {
	Kernel        Kernel    `json:"kernel"`
	Memory        Memory    `json:"memory"`
	UptimeSeconds uint64    `json:"uptimeSeconds"`
	Processes     []Process `json:"processes"`
}

type Kernel struct {
	Version string `json:"version"`
	Arch    string `json:"arch"`
	ABI     uint64 `json:"abi"`
}

type Memory struct {
	TotalMiB      uint64 `json:"totalMiB"`
	FreeMiB       uint64 `json:"freeMiB"`
	KernelHeapKiB uint64 `json:"kernelHeapKiB"`
}

type Process struct {
	ID        uint64 `json:"id"`
	Parent    uint64 `json:"parent"`
	Name      string `json:"name"`
	MemoryKiB uint64 `json:"memoryKiB"`
}

type App struct {
	ID      string `json:"id"`
	Version string `json:"version"`
	Name    string `json:"name"`
	Running bool   `json:"running"`
	// "app" or "service"; a service says whether it is enabled.
	Kind      string `json:"kind"`
	Runtime   string `json:"runtime"`
	Publisher string `json:"publisher"`
}

type Permission struct {
	Name string `json:"name"`
	// What it means, in the system's words.
	Description string `json:"description"`
	// automatic, allowed, denied or undecided.
	Decision string `json:"decision"`
	// The app's declared reason (its words, shown as a quote).
	Reason string `json:"reason"`
}

type AISession struct {
	Session uint32 `json:"session"`
	// done, needs-approval or failed.
	State string `json:"state"`
	// The answer, the reason it failed, or the approval question (worded
	// by the tool, not the model).
	Text string `json:"text"`
}

// apiError is a failure with its HTTP status and a message for the user.
type apiError struct {
	status  int
	message string
}

func (e *apiError) Error() string { return e.message }

type bridge struct {
	sys system
	// The pairing token; "" while unpaired.
	token string
	// AI sessions started here, waiting for an answer: only these may be
	// continued from here (the question, for the log).
	sessions map[uint32]string
	site     *site
	log      func(string)
	// The Store (nil: none on this system).
	store storeClient
	// Web apps (ADR-0064): their sites, the tokens their pages got, their
	// data (nil: no storage).
	webApps   map[string]*webApp
	appTokens map[string]string
	appData   appData
}

func newBridge(sys system, site *site, log func(string)) *bridge {
	return &bridge{
		sys: sys, sessions: map[uint32]string{}, site: site, log: log,
		webApps: map[string]*webApp{}, appTokens: map[string]string{},
	}
}

// pair accepts a new token (and forgets the old one and its sessions).
func (b *bridge) pair(token string) error {
	if !validToken(token) {
		return errors.New("the pairing token must be 32 to 128 hexadecimal digits")
	}
	b.token = token
	b.sessions = map[uint32]string{}
	b.appTokens = map[string]string{}
	return nil
}

func (b *bridge) unpair() bool {
	was := b.token != ""
	b.token = ""
	b.sessions = map[uint32]string{}
	// Web app pages lose their API with the pairing.
	b.appTokens = map[string]string{}
	return was
}

// sameToken compares in time independent of where the tokens differ (the
// length is not secret: every token is as long as `ui pair` makes them).
func sameToken(presented, token string) bool {
	if len(presented) != len(token) || token == "" {
		return false
	}
	var diff byte
	for i := 0; i < len(token); i++ {
		diff |= presented[i] ^ token[i]
	}
	return diff == 0
}

func validToken(token string) bool {
	if len(token) < 32 || len(token) > 128 {
		return false
	}
	return strings.Trim(strings.ToLower(token), "0123456789abcdef") == ""
}

// authorized: the request presents the current token.
func (b *bridge) authorized(req *request) bool {
	if b.token == "" {
		return false
	}
	presented := req.cookie(sessionCookie)
	if bearer, ok := strings.CutPrefix(req.get("authorization"), "Bearer "); ok {
		presented = strings.TrimSpace(bearer)
	}
	return presented != "" && sameToken(presented, b.token)
}

// serve answers one request.
func (b *bridge) serve(req *request) *response {
	var resp *response
	if req.path == "/api" || strings.HasPrefix(req.path, "/api/") {
		resp = b.api(req)
		resp.set("Cache-Control", "no-store")
	} else {
		resp = b.site.serve(req)
	}
	return harden(resp)
}

// harden adds the headers every answer carries.
func harden(resp *response) *response {
	if _, ok := resp.get("Content-Security-Policy"); !ok {
		resp.set("Content-Security-Policy", "default-src 'none'; frame-ancestors 'none'")
	}
	resp.set("X-Content-Type-Options", "nosniff")
	resp.set("X-Frame-Options", "DENY")
	resp.set("Referrer-Policy", "no-referrer")
	resp.set("Cross-Origin-Opener-Policy", "same-origin")
	resp.set("Cross-Origin-Resource-Policy", "same-origin")
	return resp
}

func (b *bridge) api(req *request) *response {
	if req.method != "GET" && req.method != "HEAD" {
		if err := sameOrigin(req); err != nil {
			return failure(err)
		}
	}
	if req.path == "/api/session" {
		return b.session(req)
	}
	if !b.authorized(req) {
		resp := failure(&apiError{401, "not paired with this browser: run `ui pair` on Oceans and enter the code"})
		resp.set("WWW-Authenticate", `Bearer realm="oceans"`)
		return resp
	}
	segments := strings.Split(strings.TrimPrefix(req.path, "/api/"), "/")
	switch {
	case req.path == "/api/system":
		return get(req, func() (any, error) { return b.sys.Info() })
	case req.path == "/api/apps":
		return get(req, func() (any, error) { return b.sys.Apps() })
	case len(segments) == 3 && segments[0] == "apps":
		return b.app(req, segments[1], segments[2])
	case req.path == "/api/audit":
		return get(req, func() (any, error) { return b.sys.Audit() })
	case req.path == "/api/ai/ask":
		return b.ask(req)
	case req.path == "/api/ai/continue":
		return b.cont(req)
	case req.path == "/api/ai/activity":
		return get(req, func() (any, error) { return b.sys.Activity() })
	case req.path == "/api/ai/model":
		return b.model(req)
	case segments[0] == "store":
		return b.storeAPI(req, segments)
	}
	return failure(&apiError{404, "no such API"})
}

// sameOrigin refuses changes requested from another site's page: a
// browser names the requesting origin, and JSON bodies cannot be sent
// across origins without a preflight this server never grants.
func sameOrigin(req *request) error {
	if site := req.get("sec-fetch-site"); site != "" && site != "same-origin" && site != "none" {
		return &apiError{403, "cross-site requests are refused"}
	}
	if origin := req.get("origin"); origin != "" && origin != "http://"+req.get("host") {
		return &apiError{403, "cross-origin requests are refused"}
	}
	if len(req.body) > 0 || req.method == "POST" {
		mediaType, _, _ := strings.Cut(req.get("content-type"), ";")
		if strings.TrimSpace(strings.ToLower(mediaType)) != "application/json" {
			return &apiError{415, "send JSON (Content-Type: application/json)"}
		}
	}
	return nil
}

func (b *bridge) session(req *request) *response {
	switch req.method {
	case "GET", "HEAD":
		return reply(200, map[string]bool{"paired": b.token != "", "authenticated": b.authorized(req)})
	case "POST":
		var body struct {
			Token string `json:"token"`
		}
		if err := decode(req.body, &body); err != nil {
			return failure(err)
		}
		token := strings.TrimSpace(body.Token)
		if b.token == "" || !sameToken(token, b.token) {
			b.log("a pairing code was refused")
			return failure(&apiError{401, "that is not the pairing code shown by `ui pair`"})
		}
		resp := &response{status: 204}
		resp.set("Set-Cookie", sessionCookie+"="+b.token+"; Path=/; HttpOnly; SameSite=Strict")
		return resp
	case "DELETE":
		resp := &response{status: 204}
		resp.set("Set-Cookie", sessionCookie+"=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
		return resp
	}
	return notAllowed("GET, POST, DELETE")
}

func (b *bridge) app(req *request, id, action string) *response {
	if !validAppID(id) {
		return failure(&apiError{400, "not an app id"})
	}
	switch action {
	case "permissions":
		return get(req, func() (any, error) { return b.sys.Permissions(id) })
	case "start":
		if req.method != "POST" {
			return notAllowed("POST")
		}
		var body struct {
			Args string `json:"args"`
		}
		if err := decode(req.body, &body); err != nil {
			return failure(err)
		}
		args := strings.TrimSpace(body.Args)
		if len(args) > 160 || !printable(args) {
			return failure(&apiError{400, "the arguments must be one short line"})
		}
		if err := b.sys.Start(id, args); err != nil {
			return failure(err)
		}
		b.log("started " + id + " for the paired browser")
		return reply(200, map[string]string{"id": id, "state": "running"})
	case "stop":
		if req.method != "POST" {
			return notAllowed("POST")
		}
		if err := decode(req.body, &struct{}{}); err != nil {
			return failure(err)
		}
		if err := b.sys.Stop(id); err != nil {
			return failure(err)
		}
		b.log("stopped " + id + " for the paired browser")
		return reply(200, map[string]string{"id": id, "state": "stopped"})
	}
	return failure(&apiError{404, "no such API"})
}

func (b *bridge) ask(req *request) *response {
	if req.method != "POST" {
		return notAllowed("POST")
	}
	var body struct {
		Question string `json:"question"`
	}
	if err := decode(req.body, &body); err != nil {
		return failure(err)
	}
	question := strings.TrimSpace(body.Question)
	if question == "" || len(question) > maxQuestion || !printable(question) {
		return failure(&apiError{400, "ask one line of at most " + strconv.Itoa(maxQuestion) + " bytes"})
	}
	session, err := b.sys.Ask(question)
	if err != nil {
		return failure(err)
	}
	b.track(session)
	return reply(200, session)
}

func (b *bridge) cont(req *request) *response {
	if req.method != "POST" {
		return notAllowed("POST")
	}
	var body struct {
		Session *uint32 `json:"session"`
		Approve *bool   `json:"approve"`
	}
	if err := decode(req.body, &body); err != nil {
		return failure(err)
	}
	if body.Session == nil || body.Approve == nil {
		return failure(&apiError{400, `"session" and "approve" are required`})
	}
	question, ok := b.sessions[*body.Session]
	if !ok {
		return failure(&apiError{404, "no AI session of this browser waits for an answer"})
	}
	// The user's explicit click in the paired browser is the approval.
	if *body.Approve {
		b.log("AI action approved by the user in the paired browser: " + question)
	} else {
		b.log("AI action denied by the user in the paired browser: " + question)
	}
	delete(b.sessions, *body.Session)
	session, err := b.sys.Continue(*body.Session, *body.Approve)
	if err != nil {
		return failure(err)
	}
	b.track(session)
	return reply(200, session)
}

// track remembers sessions waiting for this browser's answer (at most 8,
// as the AI service keeps).
func (b *bridge) track(session AISession) {
	if session.State != stateNeedsApproval {
		return
	}
	if len(b.sessions) >= 8 {
		for id := range b.sessions {
			delete(b.sessions, id)
			break
		}
	}
	b.sessions[session.Session] = session.Text
}

func (b *bridge) model(req *request) *response {
	if req.method != "POST" {
		return notAllowed("POST")
	}
	var body struct {
		URL   string `json:"url"`
		Model string `json:"model"`
	}
	if err := decode(req.body, &body); err != nil {
		return failure(err)
	}
	url, name := strings.TrimSpace(body.URL), strings.TrimSpace(body.Model)
	if url == "" || name == "" || strings.ContainsAny(url+name, " \t") || len(url)+len(name) > 200 ||
		!printable(url+name) {
		return failure(&apiError{400, "give the model server's URL and the model's name"})
	}
	note, err := b.sys.SetModel(url, name)
	if err != nil {
		return failure(err)
	}
	b.log("the paired browser set the AI model " + name + " at " + url)
	return reply(200, map[string]string{"url": url, "model": name, "note": note})
}

func get(req *request, read func() (any, error)) *response {
	if req.method != "GET" && req.method != "HEAD" {
		return notAllowed("GET")
	}
	value, err := read()
	if err != nil {
		return failure(err)
	}
	return reply(200, value)
}

func reply(status int, value any) *response {
	body, err := json.Marshal(value)
	if err != nil {
		return failure(err)
	}
	resp := &response{status: status, body: body}
	resp.set("Content-Type", "application/json; charset=utf-8")
	return resp
}

func failure(err error) *response {
	status, message := 500, "internal error"
	var api *apiError
	var bad *httpError
	switch {
	case errors.As(err, &api):
		status, message = api.status, api.message
	case errors.As(err, &bad):
		status, message = bad.status, bad.reason
	}
	body, _ := json.Marshal(map[string]string{"error": message})
	resp := &response{status: status, body: body}
	resp.set("Content-Type", "application/json; charset=utf-8")
	return resp
}

func notAllowed(allow string) *response {
	resp := failure(&apiError{405, "method not allowed"})
	resp.set("Allow", allow)
	return resp
}

// decode reads one JSON object strictly: no unknown fields, nothing
// after it. An empty body is an empty object.
func decode(body []byte, into any) error {
	body = bytes.TrimSpace(body)
	if len(body) == 0 {
		body = []byte("{}")
	}
	if body[0] != '{' {
		return &apiError{400, "malformed JSON: expected an object"}
	}
	dec := json.NewDecoder(bytes.NewReader(body))
	dec.DisallowUnknownFields()
	if err := dec.Decode(into); err != nil {
		return &apiError{400, "malformed JSON: " + err.Error()}
	}
	if dec.InputOffset() != int64(len(body)) {
		return &apiError{400, "malformed JSON: more than one value"}
	}
	return nil
}

// validAppID: an app id as packages declare them (app.oceans.hello).
func validAppID(id string) bool {
	if id == "" || len(id) > 64 {
		return false
	}
	for i := 0; i < len(id); i++ {
		c := id[i]
		if !(c >= 'a' && c <= 'z' || c >= '0' && c <= '9' || c == '.' || c == '-' || c == '_') {
			return false
		}
	}
	return true
}

// printable: no control characters (one line of text).
func printable(s string) bool {
	for _, r := range s {
		if r < ' ' || r == 0x7f {
			return false
		}
	}
	return true
}
