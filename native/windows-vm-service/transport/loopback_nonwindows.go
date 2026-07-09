//go:build !windows

package transport

import (
	"fmt"
	"net"
)

type loopbackListener struct {
	net.Listener
	identity PeerIdentity
}

// Listen creates a loopback-only development transport off Windows. It does not
// provide an OS identity boundary and must never be treated as production auth.
func Listen(config Config) (Listener, error) {
	endpoint := config.Endpoint
	if endpoint == "" {
		endpoint = "127.0.0.1:0"
	}
	address, err := net.ResolveTCPAddr("tcp", endpoint)
	if err != nil {
		return nil, fmt.Errorf("resolve loopback endpoint: %w", err)
	}
	if address.IP == nil || !address.IP.IsLoopback() {
		return nil, fmt.Errorf("development transport endpoint must use a literal loopback IP")
	}
	if config.DevelopmentPeerSID == "" {
		return nil, fmt.Errorf("development peer SID is required")
	}
	listener, err := net.ListenTCP("tcp", address)
	if err != nil {
		return nil, fmt.Errorf("listen on loopback endpoint: %w", err)
	}
	return &loopbackListener{
		Listener: listener,
		identity: PeerIdentity{
			UserSID: config.DevelopmentPeerSID,
			Method:  AuthenticationDevelopment,
		},
	}, nil
}

func (l *loopbackListener) Accept() (Conn, error) {
	connection, err := l.Listener.Accept()
	if err != nil {
		return nil, err
	}
	return &authenticatedConn{Conn: connection, identity: l.identity}, nil
}

var _ Listener = (*loopbackListener)(nil)
