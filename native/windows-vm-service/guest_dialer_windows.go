//go:build windows

package vmservice

import (
	"context"
	"net"
	"time"

	"github.com/Microsoft/go-winio"
	"github.com/Microsoft/go-winio/pkg/guid"
)

type platformGuestSocketDialer struct{}

func newPlatformGuestSocketDialer() guestSocketDialer {
	return platformGuestSocketDialer{}
}

func (platformGuestSocketDialer) Dial(ctx context.Context, runtimeID string, purpose SocketPurpose) (net.Conn, error) {
	vmID, err := guid.FromString(runtimeID)
	if err != nil {
		return nil, err
	}
	serviceIDValue, ok := socketServiceIDs[purpose]
	if !ok {
		return nil, errGuestChannelRetired
	}
	serviceID, err := guid.FromString(serviceIDValue)
	if err != nil {
		return nil, err
	}
	dialer := &winio.HvsockDialer{Retries: 60, RetryWait: 250 * time.Millisecond}
	return dialer.Dial(ctx, &winio.HvsockAddr{VMID: vmID, ServiceID: serviceID})
}
