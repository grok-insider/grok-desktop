//go:build !windows

package vmservice

import (
	"fmt"
	"os"
	"path/filepath"
	"syscall"
)

type nativePathValidator struct{}

type nativeValidatedPath struct {
	validator *nativePathValidator
	root      string
	relative  string
	kind      pathKind
	resolved  string
	identity  fileIdentity
	file      *os.File
}

func newNativePathValidator() pathValidator { return &nativePathValidator{} }

func (v *nativePathValidator) Open(root, relative string, kind pathKind) (validatedPath, error) {
	candidate := filepath.Join(root, filepath.FromSlash(relative))
	resolvedRoot, err := filepath.EvalSymlinks(root)
	if err != nil {
		return nil, err
	}
	resolved, err := filepath.EvalSymlinks(candidate)
	if err != nil {
		return nil, err
	}
	if !pathWithinRoot(resolvedRoot, resolved) {
		return nil, fmt.Errorf("resolved path escapes root")
	}
	file, err := os.Open(resolved)
	if err != nil {
		return nil, err
	}
	info, err := file.Stat()
	if err != nil {
		_ = file.Close()
		return nil, err
	}
	if kind == pathDirectory && !info.IsDir() {
		_ = file.Close()
		return nil, fmt.Errorf("path is not a directory")
	}
	if kind == pathFile && !info.Mode().IsRegular() {
		_ = file.Close()
		return nil, fmt.Errorf("path is not a regular file")
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		_ = file.Close()
		return nil, fmt.Errorf("file identity is unavailable")
	}
	return &nativeValidatedPath{
		validator: v, root: root, relative: relative, kind: kind, resolved: resolved,
		identity: fileIdentity{Volume: uint64(stat.Dev), FileLow: uint64(stat.Ino)}, file: file,
	}, nil
}

func (p *nativeValidatedPath) Close() error           { return p.file.Close() }
func (p *nativeValidatedPath) File() *os.File         { return p.file }
func (p *nativeValidatedPath) Identity() fileIdentity { return p.identity }
func (p *nativeValidatedPath) Path() string           { return p.resolved }
func (p *nativeValidatedPath) Revalidate() error {
	current, err := p.validator.Open(p.root, p.relative, p.kind)
	if err != nil {
		return err
	}
	defer current.Close()
	if current.Identity() != p.identity || current.Path() != p.resolved {
		return fmt.Errorf("path identity changed")
	}
	return nil
}
