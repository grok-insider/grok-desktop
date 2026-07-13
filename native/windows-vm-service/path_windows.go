//go:build windows

package vmservice

import (
	"encoding/binary"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unsafe"

	"golang.org/x/sys/windows"
)

const volumeNameDOS = 0

type windowsFileIDInfo struct {
	VolumeSerialNumber uint64
	FileID             [16]byte
}

type nativePathValidator struct{}

type nativeValidatedPath struct {
	validator *nativePathValidator
	root      string
	relative  string
	kind      pathKind
	resolved  string
	identity  fileIdentity
	file      *os.File
	rootFile  *os.File
}

func newNativePathValidator() pathValidator { return &nativePathValidator{} }

func (v *nativePathValidator) Open(root, relative string, kind pathKind) (validatedPath, error) {
	rootHandle, err := openWindowsHandle(root, pathDirectory)
	if err != nil {
		return nil, fmt.Errorf("open root: %w", err)
	}
	resolvedRoot, _, _, err := windowsHandleDetails(rootHandle, pathDirectory)
	if err != nil {
		_ = windows.CloseHandle(rootHandle)
		return nil, err
	}
	rootFile := os.NewFile(uintptr(rootHandle), resolvedRoot)
	if rootFile == nil {
		_ = windows.CloseHandle(rootHandle)
		return nil, fmt.Errorf("convert validated root handle")
	}

	candidate := filepath.Join(root, filepath.FromSlash(relative))
	handle, err := openWindowsHandle(candidate, kind)
	if err != nil {
		_ = rootFile.Close()
		return nil, err
	}
	resolved, identity, _, err := windowsHandleDetails(handle, kind)
	if err != nil {
		_ = windows.CloseHandle(handle)
		_ = rootFile.Close()
		return nil, err
	}
	if !windowsPathWithinRoot(resolvedRoot, resolved) {
		_ = windows.CloseHandle(handle)
		_ = rootFile.Close()
		return nil, fmt.Errorf("resolved path escapes root")
	}
	file := os.NewFile(uintptr(handle), resolved)
	if file == nil {
		_ = windows.CloseHandle(handle)
		_ = rootFile.Close()
		return nil, fmt.Errorf("convert validated handle")
	}
	return &nativeValidatedPath{
		validator: v, root: root, relative: relative, kind: kind, resolved: resolved,
		identity: identity, file: file, rootFile: rootFile,
	}, nil
}

func (p *nativeValidatedPath) Close() error {
	fileErr := p.file.Close()
	rootErr := p.rootFile.Close()
	if fileErr != nil {
		return fileErr
	}
	return rootErr
}
func (p *nativeValidatedPath) File() *os.File         { return p.file }
func (p *nativeValidatedPath) Identity() fileIdentity { return p.identity }
func (p *nativeValidatedPath) Path() string           { return p.resolved }
func (p *nativeValidatedPath) Revalidate() error {
	current, err := p.validator.Open(p.root, p.relative, p.kind)
	if err != nil {
		return err
	}
	defer current.Close()
	if current.Identity() != p.identity || !strings.EqualFold(current.Path(), p.resolved) {
		return fmt.Errorf("path identity changed")
	}
	return nil
}

func openWindowsHandle(path string, kind pathKind) (windows.Handle, error) {
	value, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return windows.InvalidHandle, err
	}
	// Request DELETE while withholding FILE_SHARE_DELETE. Besides making the
	// intended namespace exclusion explicit, this prevents directory renames on
	// Windows versions that otherwise permit MoveFileEx with an attributes-only
	// directory handle. Fail closed when the resource ACL cannot grant it.
	access := uint32(windows.FILE_READ_ATTRIBUTES | windows.DELETE)
	if kind == pathFile {
		access |= windows.GENERIC_READ
	}
	return windows.CreateFile(
		value,
		access,
		// HCS needs compatible read/write opens for VHDX and Plan9 resources.
		// Delete sharing is intentionally withheld so the validated object
		// cannot be renamed or replaced before HCS consumes its path.
		windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE,
		nil,
		windows.OPEN_EXISTING,
		windows.FILE_FLAG_BACKUP_SEMANTICS,
		0,
	)
}

func windowsHandleDetails(handle windows.Handle, kind pathKind) (string, fileIdentity, uint32, error) {
	var information windows.ByHandleFileInformation
	if err := windows.GetFileInformationByHandle(handle, &information); err != nil {
		return "", fileIdentity{}, 0, err
	}
	isDirectory := information.FileAttributes&windows.FILE_ATTRIBUTE_DIRECTORY != 0
	if kind == pathDirectory && !isDirectory {
		return "", fileIdentity{}, 0, fmt.Errorf("path is not a directory")
	}
	if kind == pathFile && isDirectory {
		return "", fileIdentity{}, 0, fmt.Errorf("path is not a file")
	}
	resolved, err := finalPath(handle)
	if err != nil {
		return "", fileIdentity{}, 0, err
	}
	var idInfo windowsFileIDInfo
	if err := windows.GetFileInformationByHandleEx(
		handle,
		windows.FileIdInfo,
		(*byte)(unsafe.Pointer(&idInfo)),
		uint32(unsafe.Sizeof(idInfo)),
	); err != nil {
		return "", fileIdentity{}, 0, err
	}
	identity := fileIdentity{
		Volume:   idInfo.VolumeSerialNumber,
		FileLow:  binary.LittleEndian.Uint64(idInfo.FileID[:8]),
		FileHigh: binary.LittleEndian.Uint64(idInfo.FileID[8:]),
	}
	return resolved, identity, information.FileAttributes, nil
}

func finalPath(handle windows.Handle) (string, error) {
	size, err := windows.GetFinalPathNameByHandle(handle, nil, 0, volumeNameDOS)
	if err != nil {
		return "", err
	}
	buffer := make([]uint16, size+1)
	written, err := windows.GetFinalPathNameByHandle(handle, &buffer[0], uint32(len(buffer)), volumeNameDOS)
	if err != nil {
		return "", err
	}
	if written == 0 || written >= uint32(len(buffer)) {
		return "", fmt.Errorf("resolved path is truncated")
	}
	return normalizeWindowsFinalPath(windows.UTF16ToString(buffer[:written])), nil
}

func normalizeWindowsFinalPath(path string) string {
	if strings.HasPrefix(path, `\\?\UNC\`) {
		return `\\` + path[len(`\\?\UNC\`):]
	}
	return strings.TrimPrefix(path, `\\?\`)
}

func windowsPathWithinRoot(root, candidate string) bool {
	relative, err := filepath.Rel(root, candidate)
	if err != nil {
		return false
	}
	if relative == "." {
		return true
	}
	return relative != ".." && !strings.HasPrefix(strings.ToLower(relative), ".."+strings.ToLower(string(filepath.Separator)))
}
