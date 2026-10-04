//go:build !wasip1

package oceans

import "testing"

func TestOffOceans(t *testing.T) {
	// Host builds are not on Oceans: no app info, every call unsupported.
	if _, err := AppInfo(); err != ErrNotAnApp {
		t.Errorf("AppInfo: %v", err)
	}
	if _, err := ReadText(1); err != ErrUnsupported {
		t.Errorf("ReadText: %v", err)
	}
}
