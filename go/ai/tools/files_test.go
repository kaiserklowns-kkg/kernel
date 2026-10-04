package tools

import (
	"testing"

	"github.com/kaiserklowns-kkg/kernel/go/ai/agent"
)

func TestFolderPaths(t *testing.T) {
	for _, good := range []string{"notes.txt", "/notes.txt", "work/plan.md", "a/b/c"} {
		if _, err := folderPath(map[string]any{"path": good}, "path", false); err != nil {
			t.Errorf("%q refused: %v", good, err)
		}
	}
	for _, bad := range []any{"", "../etc", "a/../b", "./x", "a\x00b", 7, nil} {
		if _, err := folderPath(map[string]any{"path": bad}, "path", false); err == nil {
			t.Errorf("%v accepted", bad)
		}
	}
	if path, err := folderPath(map[string]any{}, "path", true); err != nil || path != "" {
		t.Errorf("optional empty path: %q %v", path, err)
	}
}

func TestFileToolsNeedAFolderAndApproval(t *testing.T) {
	for _, tool := range []agent.Tool{filesRead{}, filesList{}} {
		if tool.Sensitivity() != agent.Reads {
			t.Errorf("%s is not a sensitive read", tool.Spec().Name)
		}
		if _, err := tool.Describe(map[string]any{"path": "notes.txt"}, agent.Env{}); err == nil {
			t.Errorf("%s described without a delegated folder", tool.Spec().Name)
		}
	}
}
