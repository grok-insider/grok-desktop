//go:build guest_control_v1_dev

package runner

import (
	"bufio"
	"context"
	"errors"
	"net"
	"sync"
	"time"
)

// Serve is intentionally compiled only for local development images. Release
// guest builds have no runtime flag that can select this unauthenticated path.
func (server *ControlServer) Serve(ctx context.Context, listener net.Listener, ready func() error) error {
	if listener == nil {
		return errors.New("control listener is required")
	}
	if ready != nil {
		if err := ready(); err != nil {
			_ = listener.Close()
			return errors.New("control readiness could not be reported")
		}
	}
	go func() {
		<-ctx.Done()
		_ = listener.Close()
	}()

	capacity := make(chan struct{}, maxControlConnections)
	var active sync.WaitGroup
	defer active.Wait()
	for {
		connection, err := listener.Accept()
		if err != nil {
			if ctx.Err() != nil || errors.Is(err, net.ErrClosed) {
				return nil
			}
			return errors.New("control listener failed")
		}
		select {
		case capacity <- struct{}{}:
			active.Add(1)
			go func() {
				defer active.Done()
				defer func() { <-capacity }()
				server.handleDevelopmentConnection(ctx, connection)
			}()
		default:
			_ = connection.Close()
		}
	}
}

func (server *ControlServer) handleDevelopmentConnection(serverContext context.Context, connection net.Conn) {
	defer connection.Close()
	_ = connection.SetDeadline(server.now().Add(15 * time.Second))
	line, err := readBoundedLine(bufio.NewReaderSize(connection, min(server.policy.MaxMessageBytes, 64<<10)), server.policy.MaxMessageBytes)
	if err != nil {
		return
	}
	response := server.Handle(serverContext, line)
	if len(response) == 0 || len(response)+1 > server.policy.MaxMessageBytes {
		response = server.encodeError("invalid", "RESOURCE_EXHAUSTED", "response exceeds the control limit")
	}
	_ = connection.SetWriteDeadline(server.now().Add(15 * time.Second))
	_ = writeAll(connection, append(response, '\n'))
}
