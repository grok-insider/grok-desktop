package workspace

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net"
	"path/filepath"
	"time"

	"github.com/grok-insider/grok-desktop/guest/runner/internal/strictjson"
)

type Client struct {
	socketPath    string
	workspaceRoot string
}

func NewClient(socketPath, workspaceRoot string) (*Client, error) {
	for _, path := range []string{socketPath, workspaceRoot} {
		if !filepath.IsAbs(path) || filepath.Clean(path) != path {
			return nil, errors.New("workspace mounter client path is invalid")
		}
	}
	return &Client{socketPath: socketPath, workspaceRoot: workspaceRoot}, nil
}

func (client *Client) Prepare(ctx context.Context, mountID, path string) error {
	if err := validateMount(client.workspaceRoot, mountID, path); err != nil {
		return err
	}
	id, err := randomID()
	if err != nil {
		return errors.New("workspace request identity could not be created")
	}
	payload, err := json.Marshal(request{
		Protocol: protocol, Type: "request", ID: id, Method: "prepare",
		MountID: mountID, Path: path,
	})
	if err != nil {
		return errors.New("workspace request could not be encoded")
	}
	connection, err := (&net.Dialer{}).DialContext(ctx, "unix", client.socketPath)
	if err != nil {
		return errors.New("workspace mounter is unavailable")
	}
	defer connection.Close()
	deadline := time.Now().Add(15 * time.Second)
	if contextDeadline, ok := ctx.Deadline(); ok && contextDeadline.Before(deadline) {
		deadline = contextDeadline
	}
	_ = connection.SetDeadline(deadline)
	if err := writeAll(connection, append(payload, '\n')); err != nil {
		return errors.New("workspace request could not be written")
	}
	line, err := readLine(bufio.NewReaderSize(connection, maximumFrameBytes))
	if err != nil {
		return errors.New("workspace response could not be read")
	}
	var result response
	if err := strictjson.Decode(line, maximumFrameBytes, &result); err != nil ||
		result.Protocol != protocol || result.Type != "response" || result.ID != id {
		return errors.New("workspace response is invalid")
	}
	if !result.OK || result.Error != nil {
		return errors.New("workspace mount was rejected")
	}
	return nil
}

func writeAll(connection net.Conn, data []byte) error {
	for len(data) > 0 {
		written, err := connection.Write(data)
		if err != nil {
			return err
		}
		if written == 0 {
			return errors.New("workspace connection made no write progress")
		}
		data = data[written:]
	}
	return nil
}

func randomID() (string, error) {
	var value [16]byte
	if _, err := rand.Read(value[:]); err != nil {
		return "", err
	}
	return hex.EncodeToString(value[:]), nil
}
