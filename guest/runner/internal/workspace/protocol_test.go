package workspace

import (
	"bufio"
	"strings"
	"testing"
)

func TestValidateMountRequiresExactBrokerPath(t *testing.T) {
	root := "/run/grok-desktop/workspaces"
	if err := validateMount(root, "project.one", root+"/project.one"); err != nil {
		t.Fatalf("valid mount was rejected: %v", err)
	}
	for _, test := range []struct {
		id   string
		path string
	}{
		{"../project", root + "/project"},
		{"Project", root + "/Project"},
		{"project", root + "/other"},
		{"project", root + "/project/child"},
	} {
		if err := validateMount(root, test.id, test.path); err == nil {
			t.Fatalf("unsafe mount was accepted: %+v", test)
		}
	}
}

func TestReadLineEnforcesFrameLimit(t *testing.T) {
	if _, err := readLine(bufio.NewReader(strings.NewReader(strings.Repeat("x", maximumFrameBytes+1) + "\n"))); err == nil {
		t.Fatal("oversized workspace frame was accepted")
	}
}
