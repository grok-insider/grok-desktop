//go:build !windows

package transport

import (
	"context"
	"net"
	"sync"
)

type memoryAddress string

func (a memoryAddress) Network() string { return "memory" }
func (a memoryAddress) String() string  { return string(a) }

type MemoryListener struct {
	queue     chan Conn
	done      chan struct{}
	closeOnce sync.Once
}

func NewMemoryListener(capacity int) *MemoryListener {
	if capacity < 1 {
		capacity = 1
	}
	return &MemoryListener{
		queue: make(chan Conn, capacity),
		done:  make(chan struct{}),
	}
}

func (l *MemoryListener) DialContext(ctx context.Context, identity PeerIdentity) (net.Conn, error) {
	client, server := net.Pipe()
	authenticated := &authenticatedConn{Conn: server, identity: identity}
	select {
	case l.queue <- authenticated:
		return client, nil
	case <-l.done:
		_ = client.Close()
		_ = server.Close()
		return nil, net.ErrClosed
	case <-ctx.Done():
		_ = client.Close()
		_ = server.Close()
		return nil, ctx.Err()
	}
}

func (l *MemoryListener) Accept() (Conn, error) {
	select {
	case conn := <-l.queue:
		if conn == nil {
			return nil, net.ErrClosed
		}
		return conn, nil
	case <-l.done:
		return nil, net.ErrClosed
	}
}

func (l *MemoryListener) Close() error {
	l.closeOnce.Do(func() { close(l.done) })
	return nil
}

func (l *MemoryListener) Addr() net.Addr {
	return memoryAddress("grok-vm-service")
}

var _ Listener = (*MemoryListener)(nil)
