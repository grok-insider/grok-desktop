//go:build windows

package main

import (
	"strings"
	"testing"
)

func TestFinalizePlatformOptionsRejectsDataRootOverride(t *testing.T) {
	options := options{dataRoot: `C:\Untrusted\VM Service`}
	err := finalizePlatformOptions(&options)
	if err == nil || !strings.Contains(err.Error(), "cannot override") {
		t.Fatalf("finalizePlatformOptions() error = %v, want fixed ProgramData root rejection", err)
	}
}
