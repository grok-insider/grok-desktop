//go:build linux

package runner

import (
	"errors"
	"fmt"
	"os"
	"path"
	"path/filepath"
	"strings"

	"github.com/grok-insider/grok-desktop/native/windows-vm-service/manifestverify"
	"golang.org/x/sys/unix"
)

const resolveFlags = unix.RESOLVE_BENEATH | unix.RESOLVE_NO_SYMLINKS | unix.RESOLVE_NO_MAGICLINKS

type SecureDir struct {
	fd       int
	ownerUID uint32
	path     string
}

func OpenSecureRoot(root string, ownerUID uint32, immutable bool) (*SecureDir, error) {
	if !filepath.IsAbs(root) || filepath.Clean(root) != root {
		return nil, errors.New("root path is not absolute and canonical")
	}
	resolved, err := filepath.EvalSymlinks(root)
	if err != nil {
		return nil, fmt.Errorf("resolve root: %w", err)
	}
	fd, err := unix.Open(resolved, unix.O_RDONLY|unix.O_DIRECTORY|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0)
	if err != nil {
		return nil, fmt.Errorf("open root: %w", err)
	}
	directory := &SecureDir{fd: fd, ownerUID: ownerUID, path: resolved}
	if err := directory.checkFD(fd, true, immutable); err != nil {
		directory.Close()
		return nil, err
	}
	return directory, nil
}

func (directory *SecureDir) Close() error {
	if directory == nil || directory.fd < 0 {
		return nil
	}
	err := unix.Close(directory.fd)
	directory.fd = -1
	return err
}

func (directory *SecureDir) Dup() (int, error) {
	fd, err := unix.FcntlInt(uintptr(directory.fd), unix.F_DUPFD_CLOEXEC, 3)
	if err != nil {
		return -1, fmt.Errorf("duplicate directory descriptor: %w", err)
	}
	return fd, nil
}

func (directory *SecureDir) Path() string {
	if directory == nil {
		return ""
	}
	return directory.path
}

func (directory *SecureDir) OpenDir(relative string, immutable bool) (*SecureDir, error) {
	if err := manifestverify.ValidateBundlePath(relative); err != nil {
		return nil, errors.New("unsafe relative directory path")
	}
	fd, err := unix.Openat2(directory.fd, relative, &unix.OpenHow{
		Flags:   unix.O_RDONLY | unix.O_DIRECTORY | unix.O_CLOEXEC | unix.O_NOFOLLOW,
		Resolve: resolveFlags,
	})
	if err != nil {
		return nil, fmt.Errorf("open directory beneath root: %w", err)
	}
	child := &SecureDir{fd: fd, ownerUID: directory.ownerUID, path: path.Join(directory.path, relative)}
	if err := child.checkFD(fd, true, immutable); err != nil {
		child.Close()
		return nil, err
	}
	return child, nil
}

func (directory *SecureDir) EnsureDir(relative string, mode uint32) (*SecureDir, error) {
	if err := manifestverify.ValidateBundlePath(relative); err != nil || !strings.Contains(relative, ".") {
		return nil, errors.New("unsafe state directory name")
	}
	if err := unix.Mkdirat(directory.fd, relative, mode); err != nil && !errors.Is(err, unix.EEXIST) {
		return nil, fmt.Errorf("create state directory: %w", err)
	}
	return directory.OpenDir(relative, false)
}

func (directory *SecureDir) OpenFile(relative string, maximum int64, immutable bool) (*os.File, os.FileInfo, error) {
	if err := manifestverify.ValidateBundlePath(relative); err != nil {
		return nil, nil, errors.New("unsafe relative file path")
	}
	fd, err := unix.Openat2(directory.fd, relative, &unix.OpenHow{
		Flags:   unix.O_RDONLY | unix.O_CLOEXEC | unix.O_NOFOLLOW,
		Resolve: resolveFlags,
	})
	if err != nil {
		return nil, nil, fmt.Errorf("open file beneath root: %w", err)
	}
	if err := directory.checkFD(fd, false, immutable); err != nil {
		unix.Close(fd)
		return nil, nil, err
	}
	file := os.NewFile(uintptr(fd), relative)
	info, err := file.Stat()
	if err != nil {
		file.Close()
		return nil, nil, fmt.Errorf("stat opened file: %w", err)
	}
	if info.Size() < 0 || info.Size() > maximum {
		file.Close()
		return nil, nil, errors.New("file exceeds its size limit")
	}
	return file, info, nil
}

func (directory *SecureDir) ReadDirNames() ([]os.DirEntry, error) {
	fd, err := directory.Dup()
	if err != nil {
		return nil, err
	}
	file := os.NewFile(uintptr(fd), directory.path)
	defer file.Close()
	entries, err := file.ReadDir(-1)
	if err != nil {
		return nil, fmt.Errorf("read directory: %w", err)
	}
	return entries, nil
}

func (directory *SecureDir) checkFD(fd int, wantDirectory, immutable bool) error {
	var stat unix.Stat_t
	if err := unix.Fstat(fd, &stat); err != nil {
		return fmt.Errorf("stat descriptor: %w", err)
	}
	mode := stat.Mode & unix.S_IFMT
	if wantDirectory && mode != unix.S_IFDIR {
		return errors.New("descriptor is not a directory")
	}
	if !wantDirectory && mode != unix.S_IFREG {
		return errors.New("descriptor is not a regular file")
	}
	if stat.Uid != directory.ownerUID {
		return errors.New("descriptor owner is not trusted")
	}
	if immutable && stat.Mode&0o022 != 0 {
		return errors.New("trusted bundle is group- or world-writable")
	}
	return nil
}
