package workspace

import (
	"bufio"
	"bytes"
	"errors"
	"path/filepath"
	"regexp"
)

const (
	protocol          = "grok.workspace-mounter/v1"
	maximumFrameBytes = 4096
	plan9Port         = 564
)

var (
	identifierPattern = regexp.MustCompile(`^[A-Za-z0-9._:-]{1,128}$`)
	mountIDPattern    = regexp.MustCompile(`^[a-z][a-z0-9.-]{0,62}$`)
)

type request struct {
	Protocol string `json:"protocol"`
	Type     string `json:"type"`
	ID       string `json:"id"`
	Method   string `json:"method"`
	MountID  string `json:"mountId"`
	Path     string `json:"path"`
}

type response struct {
	Protocol string         `json:"protocol"`
	Type     string         `json:"type"`
	ID       string         `json:"id"`
	OK       bool           `json:"ok"`
	Error    *responseError `json:"error,omitempty"`
}

type responseError struct {
	Code string `json:"code"`
}

func validateMount(root, mountID, path string) error {
	if !filepath.IsAbs(root) || filepath.Clean(root) != root || !mountIDPattern.MatchString(mountID) {
		return errors.New("workspace mount identity is invalid")
	}
	if !filepath.IsAbs(path) || filepath.Clean(path) != path || path != filepath.Join(root, mountID) {
		return errors.New("workspace mount path is invalid")
	}
	return nil
}

func readLine(reader *bufio.Reader) ([]byte, error) {
	var buffer bytes.Buffer
	for {
		fragment, more, err := reader.ReadLine()
		if err != nil {
			return nil, err
		}
		if buffer.Len()+len(fragment) > maximumFrameBytes {
			return nil, errors.New("workspace mounter frame exceeds its limit")
		}
		buffer.Write(fragment)
		if !more {
			if buffer.Len() == 0 {
				return nil, errors.New("workspace mounter frame is empty")
			}
			return buffer.Bytes(), nil
		}
	}
}
