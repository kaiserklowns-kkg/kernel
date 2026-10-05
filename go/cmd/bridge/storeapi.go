package main

import (
	"strings"

	"github.com/kaiserklowns-kkg/kernel/go/ai/httpc"
	"github.com/kaiserklowns-kkg/kernel/go/store"
)

// The Store (ADR-0061): the catalog of the store the user chose, and
// installs proposed to Oceans Core, which the user confirms on the device.
// The browser never installs anything itself.

// storeClient reaches the store (store.go on Oceans, a fake in tests).
type storeClient interface {
	// Source is the store's URL ("" if none is set).
	Source() string
	SetSource(url string) error
	Catalog() (store.Catalog, error)
	Download(app store.App) ([]byte, error)
}

// StoreApp is a catalog entry and its state here.
type StoreApp struct {
	store.App
	// available, installed or update.
	State string `json:"state"`
	// The installed version, if any.
	Installed string `json:"installed,omitempty"`
}

// StoreView is what GET /api/store answers.
type StoreView struct {
	Source string     `json:"source"`
	Apps   []StoreApp `json:"apps"`
}

var errNoStore = &apiError{503, "the Store is not available on this system"}

func (b *bridge) storeAPI(req *request, segments []string) *response {
	if b.store == nil {
		return failure(errNoStore)
	}
	switch {
	case len(segments) == 1:
		return get(req, b.storeView)
	case len(segments) == 2 && segments[1] == "source":
		return b.storeSource(req)
	case len(segments) == 2 && segments[1] == "install":
		return b.storeInstall(req)
	}
	return failure(&apiError{404, "no such API"})
}

func (b *bridge) storeView() (any, error) {
	view := StoreView{Source: b.store.Source(), Apps: []StoreApp{}}
	if view.Source == "" {
		return view, nil
	}
	catalog, err := b.store.Catalog()
	if err != nil {
		return nil, &apiError{502, "the store: " + err.Error()}
	}
	installed := map[string]string{}
	if apps, err := b.sys.Apps(); err == nil {
		for _, app := range apps {
			installed[app.ID] = app.Version
		}
	}
	for _, app := range catalog.Apps {
		view.Apps = append(view.Apps, StoreApp{
			App:       app,
			State:     store.StateOf(app.Version, installed[app.ID]),
			Installed: installed[app.ID],
		})
	}
	return view, nil
}

func (b *bridge) storeSource(req *request) *response {
	if req.method != "POST" {
		return notAllowed("POST")
	}
	var body struct {
		URL string `json:"url"`
	}
	if err := decode(req.body, &body); err != nil {
		return failure(err)
	}
	url := strings.TrimSpace(body.URL)
	if url != "" {
		if _, err := httpc.ParseEndpoint(url); err != nil || len(url) > 200 || !printable(url) {
			return failure(&apiError{400, "give the store's http:// or https:// URL"})
		}
	}
	if err := b.store.SetSource(url); err != nil {
		return failure(err)
	}
	if url == "" {
		b.log("the paired browser removed the Store's source")
	} else {
		b.log("the paired browser set the Store's source to " + url)
	}
	return reply(200, map[string]string{"source": url})
}

func (b *bridge) storeInstall(req *request) *response {
	if req.method != "POST" {
		return notAllowed("POST")
	}
	var body struct {
		ID string `json:"id"`
	}
	if err := decode(req.body, &body); err != nil {
		return failure(err)
	}
	if !validAppID(body.ID) {
		return failure(&apiError{400, "not an app id"})
	}
	if b.store.Source() == "" {
		return failure(&apiError{409, "no store is set"})
	}
	catalog, err := b.store.Catalog()
	if err != nil {
		return failure(&apiError{502, "the store: " + err.Error()})
	}
	app, ok := catalog.Find(body.ID)
	if !ok {
		return failure(&apiError{404, "the store does not list that app"})
	}
	pkg, err := b.store.Download(app)
	if err != nil {
		return failure(&apiError{502, "the store: " + err.Error()})
	}
	if err := app.Verify(pkg); err != nil {
		return failure(&apiError{502, "the store: " + err.Error()})
	}
	if err := b.sys.Propose(pkg); err != nil {
		return failure(err)
	}
	b.log("proposed installing " + app.ID + " " + app.Version + " from the Store; the user confirms on the device")
	return reply(202, map[string]string{"id": app.ID, "state": "confirm on the device"})
}
