package main

import (
	"github.com/kaiserklowns-kkg/kernel/go/oceans/fs"
)

// oceansAppData keeps web apps' data in the bridge's storage, one
// directory per app: apps/ID/NAME (ADR-0064).
type oceansAppData struct {
	storage fs.Node
}

func (d oceansAppData) dir(id string, create bool) (fs.Node, error) {
	flags := uint8(0)
	if create {
		flags = fs.CreateDirectory | fs.Write
	}
	apps, _, err := d.storage.Open("apps", flags)
	if err != nil {
		return fs.Node{}, err
	}
	defer apps.Close()
	dir, _, err := apps.Open(id, flags)
	return dir, err
}

func (d oceansAppData) Get(id, name string) ([]byte, error) {
	dir, err := d.dir(id, false)
	if err != nil {
		return nil, errNoData
	}
	defer dir.Close()
	file, kind, err := dir.Open(name, 0)
	if err != nil || kind != fs.File {
		return nil, errNoData
	}
	defer file.Close()
	return file.ReadAll(maxAppData)
}

func (d oceansAppData) Put(id, name string, data []byte) error {
	dir, err := d.dir(id, true)
	if err != nil {
		return &apiError{500, "storage failed: " + err.Error()}
	}
	defer dir.Close()
	if err := dir.WriteFile(name, data); err != nil {
		return &apiError{500, "storage failed: " + err.Error()}
	}
	return nil
}
