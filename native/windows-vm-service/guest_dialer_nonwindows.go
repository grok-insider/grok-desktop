//go:build !windows

package vmservice

import (
	"context"
	"errors"
	"net"
)

type platformGuestSocketDialer struct{}

func newPlatformGuestSocketDialer() guestSocketDialer {
	return platformGuestSocketDialer{}
}

func (platformGuestSocketDialer) Dial(context.Context, string, SocketPurpose) (net.Conn, error) {
	return nil, errors.New("Hyper-V sockets are unavailable on this platform")
}
