//go:build windows

package vmservice

import (
	"os"
	"path/filepath"

	"golang.org/x/sys/windows"
)

func pathComponentIsRedirecting(path string, _ os.FileInfo) (bool, error) {
	pathPointer, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return false, err
	}
	attributes, err := windows.GetFileAttributes(pathPointer)
	if err != nil {
		return false, err
	}
	return attributes&windows.FILE_ATTRIBUTE_REPARSE_POINT != 0, nil
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
	overlapped := new(windows.Overlapped)
	if err := windows.LockFileEx(windows.Handle(lockFile.Fd()), windows.LOCKFILE_EXCLUSIVE_LOCK, 0, 1, 0, overlapped); err != nil {
		return serviceError(CodeUnavailable, "acquire guest image policy lock: %v", err)
	}
	defer func() {
		_ = windows.UnlockFileEx(windows.Handle(lockFile.Fd()), 0, 1, 0, overlapped)
	}()
	return operation()
}
