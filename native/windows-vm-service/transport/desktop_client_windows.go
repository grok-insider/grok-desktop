//go:build windows

package transport

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	errorSuccess              = uint32(0)
	errorInsufficientBuffer   = uint32(122)
	appModelErrorNoPackage    = uint32(15700)
	maximumPackageIdentityLen = uint32(1024)
	maximumProcessPathLen     = uint32(32768)
)

var (
	clientKernel32           = windows.NewLazySystemDLL("kernel32.dll")
	procGetPackageFullName   = clientKernel32.NewProc("GetPackageFullName")
	procGetPackageFamilyName = clientKernel32.NewProc("GetPackageFamilyName")
)

type desktopClientQualifier struct {
	policy desktopClientPolicy
}

type clientProcessIdentity struct {
	processID  uint32
	startedAt  uint64
	packaged   bool
	guestGrant bool
}

func newDesktopClientQualifier() (*desktopClientQualifier, error) {
	executable, err := os.Executable()
	if err != nil {
		return nil, fmt.Errorf("resolve VM service executable: %w", err)
	}
	resourcesRoot := filepath.Dir(filepath.Dir(executable))
	expectedDaemon := filepath.Clean(filepath.Join(resourcesRoot, "bin", "grok-daemon.exe"))
	fullName, familyName, packaged, err := packageIdentity(windows.CurrentProcess())
	if err != nil {
		return nil, fmt.Errorf("read VM service package identity: %w", err)
	}
	return &desktopClientQualifier{policy: desktopClientPolicy{
		executablePath: expectedDaemon,
		packageFull:    fullName,
		packageFamily:  familyName,
		packaged:       packaged,
	}}, nil
}

func (qualifier *desktopClientQualifier) identify(pipe windows.Handle) (clientProcessIdentity, error) {
	var processID uint32
	if err := windows.GetNamedPipeClientProcessId(pipe, &processID); err != nil || processID == 0 {
		return clientProcessIdentity{}, fmt.Errorf("read named-pipe client process")
	}
	process, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION, false, processID)
	if err != nil {
		return clientProcessIdentity{}, fmt.Errorf("open named-pipe client process")
	}
	defer windows.CloseHandle(process)

	var created, exited, kernel, user windows.Filetime
	if err := windows.GetProcessTimes(process, &created, &exited, &kernel, &user); err != nil {
		return clientProcessIdentity{}, fmt.Errorf("read named-pipe client process identity")
	}
	startedAt := uint64(created.HighDateTime)<<32 | uint64(created.LowDateTime)
	if startedAt == 0 {
		return clientProcessIdentity{}, fmt.Errorf("named-pipe client process identity is empty")
	}
	executable, err := processExecutable(process)
	if err != nil {
		return clientProcessIdentity{}, err
	}
	fullName, familyName, packaged, err := packageIdentity(process)
	if err != nil {
		return clientProcessIdentity{}, fmt.Errorf("read named-pipe client package identity")
	}
	qualified := qualifier.policy.qualifies(executable, fullName, familyName, packaged)
	return clientProcessIdentity{
		processID: processID,
		startedAt: startedAt,
		packaged:  qualified,
		// Package and process continuity are necessary but insufficient. The
		// daemon proof-of-possession handshake promotes this grant later.
		guestGrant: false,
	}, nil
}

func processExecutable(process windows.Handle) (string, error) {
	buffer := make([]uint16, maximumProcessPathLen)
	size := uint32(len(buffer))
	if err := windows.QueryFullProcessImageName(process, 0, &buffer[0], &size); err != nil || size == 0 || size >= uint32(len(buffer)) {
		return "", fmt.Errorf("read named-pipe client executable")
	}
	return filepath.Clean(windows.UTF16ToString(buffer[:size])), nil
}

func packageIdentity(process windows.Handle) (string, string, bool, error) {
	fullName, fullPresent, err := packageIdentityValue(procGetPackageFullName, process)
	if err != nil {
		return "", "", false, err
	}
	familyName, familyPresent, err := packageIdentityValue(procGetPackageFamilyName, process)
	if err != nil {
		return "", "", false, err
	}
	if !fullPresent || !familyPresent {
		return "", "", false, nil
	}
	if strings.TrimSpace(fullName) == "" || strings.TrimSpace(familyName) == "" {
		return "", "", false, fmt.Errorf("package identity is empty")
	}
	return fullName, familyName, true, nil
}

func packageIdentityValue(procedure *windows.LazyProc, process windows.Handle) (string, bool, error) {
	var length uint32
	result, _, _ := procedure.Call(
		uintptr(process),
		uintptr(unsafe.Pointer(&length)),
		0,
	)
	code := uint32(result)
	if code == appModelErrorNoPackage {
		return "", false, nil
	}
	if code != errorInsufficientBuffer || length < 2 || length > maximumPackageIdentityLen {
		return "", false, fmt.Errorf("package identity length query failed with code %d", code)
	}
	buffer := make([]uint16, length)
	result, _, _ = procedure.Call(
		uintptr(process),
		uintptr(unsafe.Pointer(&length)),
		uintptr(unsafe.Pointer(&buffer[0])),
	)
	if code = uint32(result); code != errorSuccess || length < 2 || length > uint32(len(buffer)) {
		return "", false, fmt.Errorf("package identity query failed with code %d", code)
	}
	value := windows.UTF16ToString(buffer[:length])
	if value == "" {
		return "", false, fmt.Errorf("package identity query returned an empty value")
	}
	return value, true, nil
}
