//go:build windows

package vmservice

import (
	"os"
	"path/filepath"
	"testing"
)

func TestWindowsValidatedHandleBlocksReplacement(t *testing.T) {
	root := t.TempDir()
	for _, test := range []struct {
		name string
		kind pathKind
	}{
		{"disk.vhdx", pathFile},
		{"workspace", pathDirectory},
	} {
		t.Run(test.name, func(t *testing.T) {
			path := filepath.Join(root, test.name)
			if test.kind == pathDirectory {
				if err := os.Mkdir(path, 0o700); err != nil {
					t.Fatal(err)
				}
			} else if err := os.WriteFile(path, []byte("vhdx"), 0o600); err != nil {
				t.Fatal(err)
			}
			validated, err := newNativePathValidator().Open(root, test.name, test.kind)
			if err != nil {
				t.Fatal(err)
			}
			defer validated.Close()
			if err := os.Rename(path, filepath.Join(root, test.name+".replaced")); err == nil {
				t.Fatal("validated resource was renamed while its no-delete-share handle was open")
			}
		})
	}
}
