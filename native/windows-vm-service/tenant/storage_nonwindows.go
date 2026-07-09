//go:build !windows

package tenant

import (
	"os"
	"path/filepath"
)

func secureServiceStorageRoot(root string) error {
	if err := os.MkdirAll(root, 0o700); err != nil {
		return err
	}
	return os.Chmod(root, 0o700)
}

func secureTenantStorage(roots StorageRoots, _ string) error {
	if err := secureServiceStorageRoot(filepath.Dir(filepath.Dir(roots.TenantRoot))); err != nil {
		return err
	}
	for _, directory := range []string{roots.TenantRoot, roots.ImageRoot, roots.StagingRoot, roots.WorkspaceRoot} {
		if err := os.MkdirAll(directory, 0o700); err != nil {
			return err
		}
		if err := os.Chmod(directory, 0o700); err != nil {
			return err
		}
	}
	return nil
}
