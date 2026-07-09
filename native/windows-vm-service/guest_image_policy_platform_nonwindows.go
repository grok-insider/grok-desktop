//go:build !windows

package vmservice

import (
	"os"
	"path/filepath"

	"golang.org/x/sys/unix"
)

func pathComponentIsRedirecting(_ string, information os.FileInfo) (bool, error) {
	return information.Mode()&os.ModeSymlink != 0, nil
}

func withGuestImagePolicyLock(root string, operation func() error) error {
	lockPath := filepath.Join(root, ".guest-image-policy.lock")
	if err := rejectRedirectingPathIfPresent(root, ".guest-image-policy.lock"); err != nil {
		return serviceError(CodeUnavailable, "guest image policy lock path is unsafe")
	}
	lockFile, err := os.OpenFile(lockPath, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return serviceError(CodeUnavailable, "open guest image policy lock: %v", err)
	}
	defer lockFile.Close()
	if err := unix.Flock(int(lockFile.Fd()), unix.LOCK_EX); err != nil {
		return serviceError(CodeUnavailable, "acquire guest image policy lock: %v", err)
	}
	defer func() { _ = unix.Flock(int(lockFile.Fd()), unix.LOCK_UN) }()
	return operation()
}
