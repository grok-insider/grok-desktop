//go:build !guest_control_v1_dev

package runner

import (
	"context"
	"encoding/json"
	"errors"
	"net"

	"github.com/grok-insider/grok-desktop/guest/runner/internal/strictjson"
	guestchannelv2 "github.com/grok-insider/grok-desktop/native/windows-vm-service/guestchannel/v2"
)

func (server *ControlServer) Serve(ctx context.Context, listener net.Listener, ready func() error) error {
	if listener == nil {
		return errors.New("control listener is required")
	}
	defer listener.Close()
	go func() {
		<-ctx.Done()
		_ = listener.Close()
	}()

	for {
		connection, err := listener.Accept()
		if err != nil {
			if ctx.Err() != nil || errors.Is(err, net.ErrClosed) {
				return nil
			}
			return errors.New("control listener failed")
		}
		channel, err := guestchannelv2.AcceptGuest(
			ctx,
			connection,
			server.policy.MaxMessageBytes,
			server.handshakeTimeout,
		)
		if err != nil {
			_ = connection.Close()
			if ctx.Err() != nil {
				return nil
			}
			continue
		}
		_ = listener.Close()
		if ready != nil {
			if err := ready(); err != nil {
				_ = channel.Close()
				return errors.New("control readiness could not be reported")
			}
		}
		return server.serveAuthenticated(ctx, connection, channel)
	}
}

func (server *ControlServer) serveAuthenticated(ctx context.Context, connection net.Conn, channel *guestchannelv2.GuestChannel) error {
	defer channel.Close()
	closed := make(chan struct{})
	defer close(closed)
	go func() {
		select {
		case <-ctx.Done():
			_ = connection.Close()
		case <-closed:
		}
	}()

	for {
		request, err := channel.Receive(ctx)
		if errors.Is(err, guestchannelv2.ErrReplayServed) {
			continue
		}
		if err != nil {
			if ctx.Err() != nil {
				return nil
			}
			return errors.New("authenticated control receive failed")
		}
		controlFrame := request.ControlFrame()
		if !authenticatedMetadataMatches(controlFrame, request) {
			zeroControlBytes(controlFrame)
			return errors.New("authenticated control metadata did not match its payload")
		}
		response := server.Handle(ctx, controlFrame)
		zeroControlBytes(controlFrame)
		if len(response) == 0 || len(response) > server.policy.MaxMessageBytes {
			zeroControlBytes(response)
			return errors.New("authenticated control response exceeded its bound")
		}
		if err := channel.Respond(ctx, request, response); err != nil {
			zeroControlBytes(response)
			if ctx.Err() != nil {
				return nil
			}
			return errors.New("authenticated control response failed")
		}
		zeroControlBytes(response)
	}
}

func authenticatedMetadataMatches(data []byte, authenticated *guestchannelv2.GuestRequest) bool {
	if authenticated == nil {
		return false
	}
	var request controlRequest
	if err := strictjson.Decode(data, int64(len(data)), &request); err != nil ||
		request.Protocol != controlProtocol || request.Type != "request" ||
		!requestIDPattern.MatchString(request.ID) || request.DeadlineUnixMS <= 0 ||
		len(request.Params) == 0 || !json.Valid(request.Params) {
		return false
	}
	return request.ID == authenticated.RequestID() && uint64(request.DeadlineUnixMS) == authenticated.DeadlineUnixMS()
}

func zeroControlBytes(data []byte) {
	for index := range data {
		data[index] = 0
	}
}
