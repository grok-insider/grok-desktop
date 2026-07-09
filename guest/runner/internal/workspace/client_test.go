package workspace

import (
	"bufio"
	"context"
	"encoding/json"
	"net"
	"path/filepath"
	"testing"
	"time"

	"github.com/grok-insider/grok-desktop/guest/runner/internal/strictjson"
)

func TestClientUsesBoundedCorrelatedProtocol(t *testing.T) {
	socketPath := filepath.Join(t.TempDir(), "mounter.sock")
	listener, err := net.ListenUnix("unix", &net.UnixAddr{Name: socketPath, Net: "unix"})
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	serverDone := make(chan error, 1)
	go func() {
		connection, err := listener.AcceptUnix()
		if err != nil {
			serverDone <- err
			return
		}
		defer connection.Close()
		line, err := readLine(bufio.NewReader(connection))
		if err != nil {
			serverDone <- err
			return
		}
		var message request
		if err := strictjson.Decode(line, maximumFrameBytes, &message); err != nil {
			serverDone <- err
			return
		}
		payload, err := json.Marshal(response{Protocol: protocol, Type: "response", ID: message.ID, OK: true})
		if err == nil {
			err = writeAll(connection, append(payload, '\n'))
		}
		serverDone <- err
	}()

	root := "/run/grok-desktop/workspaces"
	client, err := NewClient(socketPath, root)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := client.Prepare(ctx, "project", root+"/project"); err != nil {
		t.Fatal(err)
	}
	if err := <-serverDone; err != nil {
		t.Fatal(err)
	}
}
