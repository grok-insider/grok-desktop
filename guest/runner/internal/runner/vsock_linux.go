//go:build linux

package runner

import (
	"errors"
	"net"

	"github.com/mdlayher/vsock"
	"golang.org/x/sys/unix"
)

type hostOnlyListener struct {
	net.Listener
}

func ListenHostVSock(port uint32) (net.Listener, error) {
	if port != defaultPort {
		return nil, errors.New("control port is not allowlisted")
	}
	// Bind CID_ANY directly. vsock.Listen probes /dev/vsock to discover the
	// local CID, but the system service intentionally runs with PrivateDevices.
	listener, err := vsock.ListenContextID(unix.VMADDR_CID_ANY, port, nil)
	if err != nil {
		return nil, errors.New("AF_VSOCK control listener could not be created")
	}
	return &hostOnlyListener{Listener: listener}, nil
}

func (listener *hostOnlyListener) Accept() (net.Conn, error) {
	for {
		connection, err := listener.Listener.Accept()
		if err != nil {
			return nil, err
		}
		if isHostVSockAddress(connection.RemoteAddr()) {
			return connection, nil
		}
		_ = connection.Close()
	}
}

func isHostVSockAddress(address net.Addr) bool {
	peer, ok := address.(*vsock.Addr)
	return ok && peer.ContextID == vsock.Host
}
