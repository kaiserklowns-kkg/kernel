package tools

import (
	"errors"
	"fmt"
	"strings"
	"unicode/utf8"

	"github.com/kaiserklowns-kkg/kernel/go/ai/agent"
	"github.com/kaiserklowns-kkg/kernel/go/ai/model"
	"github.com/kaiserklowns-kkg/kernel/go/oceans/fs"
)

// Sensitive reads (ADR-0055): the user's files, through the folder the
// requester delegated to the session ("files", read-only). Each call
// needs the user's approval, worded with the exact path; nothing outside
// the folder can be named (the capability has no parent).

// Largest file the agent may read, and how much of it reaches the model.
const (
	maxFileRead  = 64 << 10
	maxFileShown = 4000
	maxListed    = 100
)

// folderPath checks a path relative to the delegated folder.
func folderPath(args map[string]any, key string, optional bool) (string, error) {
	path, _ := args[key].(string)
	path = strings.Trim(path, "/")
	if path == "" {
		if optional {
			return "", nil
		}
		return "", fmt.Errorf("%q must name a file in the folder, e.g. notes.txt", key)
	}
	if len(path) > 200 {
		return "", errors.New("the path is too long")
	}
	for _, part := range strings.Split(path, "/") {
		if !fs.ValidName(part) {
			return "", fmt.Errorf("%q is not a valid path inside the folder", path)
		}
	}
	return path, nil
}

func folder(env agent.Env) (fs.Node, error) {
	h, err := handle(env, "files")
	if err != nil {
		return fs.Node{}, err
	}
	return fs.FromHandle(h), nil
}

type filesList struct{}

func (filesList) Spec() model.Function {
	return model.Function{
		Name:        "files_list",
		Description: "List the files in the user's folder (or a folder inside it). Needs the user's approval.",
		Parameters: []byte(`{"type":"object","properties":{` +
			`"path":{"type":"string","description":"a folder inside the user's folder; empty for the folder itself"}}}`),
	}
}
func (filesList) Sensitivity() agent.Sensitivity { return agent.Reads }
func (filesList) Describe(args map[string]any, env agent.Env) (string, error) {
	path, err := folderPath(args, "path", true)
	if err != nil {
		return "", err
	}
	if _, err := folder(env); err != nil {
		return "", err
	}
	if path == "" {
		return "see the names of the files in your folder", nil
	}
	return fmt.Sprintf("see the names of the files in %s in your folder", path), nil
}
func (filesList) Run(args map[string]any, env agent.Env) (string, error) {
	path, err := folderPath(args, "path", true)
	if err != nil {
		return "", err
	}
	root, err := folder(env)
	if err != nil {
		return "", err
	}
	dir := root
	if path != "" {
		node, kind, err := root.Walk(path, 0)
		if err != nil {
			return "", err
		}
		defer node.Close()
		if kind != fs.Directory {
			return "", errors.New(path + " is not a folder")
		}
		dir = node
	}
	entries, err := dir.List(maxListed)
	if err != nil {
		return "", err
	}
	if len(entries) == 0 {
		return "the folder is empty", nil
	}
	var out strings.Builder
	for _, e := range entries {
		out.WriteString(e.Name)
		if e.Kind == fs.Directory {
			out.WriteByte('/')
		}
		out.WriteByte('\n')
	}
	return strings.TrimSpace(out.String()), nil
}

type filesRead struct{}

func (filesRead) Spec() model.Function {
	return model.Function{
		Name:        "files_read",
		Description: "Read a text file in the user's folder. Needs the user's approval.",
		Parameters: []byte(`{"type":"object","properties":{` +
			`"path":{"type":"string","description":"the file, inside the user's folder, e.g. notes.txt"}},"required":["path"]}`),
	}
}
func (filesRead) Sensitivity() agent.Sensitivity { return agent.Reads }
func (filesRead) Describe(args map[string]any, env agent.Env) (string, error) {
	path, err := folderPath(args, "path", false)
	if err != nil {
		return "", err
	}
	if _, err := folder(env); err != nil {
		return "", err
	}
	return fmt.Sprintf("read the file %s in your folder", path), nil
}
func (filesRead) Run(args map[string]any, env agent.Env) (string, error) {
	path, err := folderPath(args, "path", false)
	if err != nil {
		return "", err
	}
	root, err := folder(env)
	if err != nil {
		return "", err
	}
	file, kind, err := root.Walk(path, 0)
	if err != nil {
		return "", err
	}
	defer file.Close()
	if kind != fs.File {
		return "", errors.New(path + " is a folder")
	}
	data, err := file.ReadAll(maxFileRead)
	if err != nil {
		return "", err
	}
	if !utf8.Valid(data) {
		return "", errors.New(path + " is not a text file")
	}
	text := string(data)
	if len(text) > maxFileShown {
		text = text[:maxFileShown] + "\n[... cut: the file is longer]"
	}
	return text, nil
}
