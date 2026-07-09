//go:build windows

package vmservice

import (
	"os"
	"path/filepath"
	"testing"
)

func TestWindowsAtomicReplaceCreatesAndReplaces(t *testing.T) {
	directory := t.TempDir()
	destination := filepath.Join(directory, "state.json")

	first := filepath.Join(directory, "first.tmp")
	if err := os.WriteFile(first, []byte("first"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := atomicReplace(first, destination); err != nil {
		t.Fatalf("first publish: %v", err)
	}

	second := filepath.Join(directory, "second.tmp")
	if err := os.WriteFile(second, []byte("second"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := atomicReplace(second, destination); err != nil {
		t.Fatalf("replacement publish: %v", err)
	}
	contents, err := os.ReadFile(destination)
	if err != nil {
		t.Fatal(err)
	}
	if string(contents) != "second" {
		t.Fatalf("published contents = %q", contents)
	}
	if _, err := os.Stat(second); !os.IsNotExist(err) {
		t.Fatalf("source remains after publish: %v", err)
	}
}
