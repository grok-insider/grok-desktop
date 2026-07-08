//go:build !guest_control_v1_dev

package runner

import (
	"context"
	"net"
	"sync"
	"testing"
	"time"

	guestchannelv2 "github.com/grok-insider/grok-desktop/native/windows-vm-service/guestchannel/v2"
)

func TestControlServerRequiresProvisioningBeforeReadinessAndServesV2(t *testing.T) {
	manager := &fakeControlManager{}
	server, now := newTestControlServer(t, manager)
	hostConnection, guestConnection := net.Pipe()
	listener := newOneConnectionListener(guestConnection)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	ready := make(chan struct{}, 1)
	serveResult := make(chan error, 1)
	go func() {
		serveResult <- server.Serve(ctx, listener, func() error {
			ready <- struct{}{}
			return nil
		})
	}()

	select {
	case <-ready:
		t.Fatal("guest reported readiness before channel provisioning")
	case <-time.After(20 * time.Millisecond):
	}
	material, err := guestchannelv2.NewBootMaterial()
	if err != nil {
		t.Fatal(err)
	}
	host, err := guestchannelv2.ProvisionHost(context.Background(), hostConnection, material, server.policy.MaxMessageBytes, time.Second)
	if err != nil {
		t.Fatalf("ProvisionHost: %v", err)
	}
	defer host.Close()
	select {
	case <-ready:
	case <-time.After(time.Second):
		t.Fatal("guest did not report readiness after authenticated provisioning")
	}

	request := controlRequestJSON(t, "health-v2", "runner.health", `{}`, now.Add(time.Minute))
	responseBytes, err := host.RoundTrip(context.Background(), "health-v2", now.Add(time.Minute), request)
	if err != nil {
		t.Fatalf("RoundTrip: %v", err)
	}
	response := decodeControlResponse(t, responseBytes)
	if !response.OK || response.ID != "health-v2" {
		t.Fatalf("authenticated health response = %+v", response)
	}

	cancel()
	select {
	case err := <-serveResult:
		if err != nil {
			t.Fatalf("Serve shutdown: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("authenticated control server did not stop")
	}
}

func TestControlServerAuthenticatesBeforeControlJSONDecode(t *testing.T) {
	manager := &fakeControlManager{}
	server, now := newTestControlServer(t, manager)
	hostPipe, guestConnection := net.Pipe()
	hostConnection := &tamperWriteConnection{Conn: hostPipe}
	listener := newOneConnectionListener(guestConnection)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	ready := make(chan struct{}, 1)
	serveResult := make(chan error, 1)
	go func() {
		serveResult <- server.Serve(ctx, listener, func() error {
			ready <- struct{}{}
			return nil
		})
	}()
	material, err := guestchannelv2.NewBootMaterial()
	if err != nil {
		t.Fatal(err)
	}
	host, err := guestchannelv2.ProvisionHost(context.Background(), hostConnection, material, server.policy.MaxMessageBytes, time.Second)
	if err != nil {
		t.Fatalf("ProvisionHost: %v", err)
	}
	defer host.Close()
	<-ready
	hostConnection.enableTamper()
	request := controlRequestJSON(t, "catalog-v2", "catalog.apply", `{"catalog":{"version":1}}`, now.Add(time.Minute))
	if _, err := host.RoundTrip(context.Background(), "catalog-v2", now.Add(time.Minute), request); err == nil {
		t.Fatal("tampered authenticated frame received a response")
	}
	select {
	case err := <-serveResult:
		if err == nil {
			t.Fatal("tampered channel did not fail closed")
		}
	case <-time.After(time.Second):
		t.Fatal("tampered channel remained open")
	}
	manager.mu.Lock()
	calls := manager.applyCalls
	manager.mu.Unlock()
	if calls != 0 {
		t.Fatalf("control JSON dispatched before MAC verification: %d calls", calls)
	}
}

func TestControlServerRejectsAuthenticatedMetadataMismatch(t *testing.T) {
	manager := &fakeControlManager{}
	server, now := newTestControlServer(t, manager)
	hostConnection, guestConnection := net.Pipe()
	listener := newOneConnectionListener(guestConnection)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	serveResult := make(chan error, 1)
	go func() { serveResult <- server.Serve(ctx, listener, nil) }()
	material, err := guestchannelv2.NewBootMaterial()
	if err != nil {
		t.Fatal(err)
	}
	host, err := guestchannelv2.ProvisionHost(context.Background(), hostConnection, material, server.policy.MaxMessageBytes, time.Second)
	if err != nil {
		t.Fatalf("ProvisionHost: %v", err)
	}
	defer host.Close()
	request := controlRequestJSON(t, "inner-id", "catalog.apply", `{"catalog":{"version":1}}`, now.Add(time.Minute))
	if _, err := host.RoundTrip(context.Background(), "outer-id", now.Add(time.Minute), request); err == nil {
		t.Fatal("mismatched authenticated request ID received a response")
	}
	select {
	case err := <-serveResult:
		if err == nil {
			t.Fatal("metadata mismatch did not fail the channel")
		}
	case <-time.After(time.Second):
		t.Fatal("metadata mismatch did not close the channel")
	}
	manager.mu.Lock()
	calls := manager.applyCalls
	manager.mu.Unlock()
	if calls != 0 {
		t.Fatalf("metadata mismatch reached manager: %d calls", calls)
	}
}

func TestControlServerProvisioningTimeoutDoesNotReportReady(t *testing.T) {
	server, _ := newTestControlServer(t, &fakeControlManager{})
	server.handshakeTimeout = 20 * time.Millisecond
	hostConnection, guestConnection := net.Pipe()
	defer hostConnection.Close()
	listener := newOneConnectionListener(guestConnection)
	ctx, cancel := context.WithCancel(context.Background())
	ready := make(chan struct{}, 1)
	serveResult := make(chan error, 1)
	go func() {
		serveResult <- server.Serve(ctx, listener, func() error {
			ready <- struct{}{}
			return nil
		})
	}()
	time.Sleep(80 * time.Millisecond)
	select {
	case <-ready:
		t.Fatal("timed-out provisioning reported guest readiness")
	default:
	}
	cancel()
	select {
	case err := <-serveResult:
		if err != nil {
			t.Fatalf("Serve shutdown: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("server did not leave provisioning after cancellation")
	}
}

type oneConnectionListener struct {
	connection net.Conn
	closed     chan struct{}
	closeOnce  sync.Once
	acceptOnce sync.Once
	accepted   bool
}

func newOneConnectionListener(connection net.Conn) *oneConnectionListener {
	return &oneConnectionListener{connection: connection, closed: make(chan struct{})}
}

func (listener *oneConnectionListener) Accept() (net.Conn, error) {
	listener.acceptOnce.Do(func() { listener.accepted = true })
	if listener.accepted {
		connection := listener.connection
		listener.connection = nil
		listener.accepted = false
		return connection, nil
	}
	<-listener.closed
	return nil, net.ErrClosed
}

func (listener *oneConnectionListener) Close() error {
	listener.closeOnce.Do(func() { close(listener.closed) })
	return nil
}

func (*oneConnectionListener) Addr() net.Addr { return pipeAddress("guest-v2") }

type pipeAddress string

func (address pipeAddress) Network() string { return "pipe" }
func (address pipeAddress) String() string  { return string(address) }

type tamperWriteConnection struct {
	net.Conn
	mu       sync.Mutex
	tamper   bool
	tampered bool
}

func (connection *tamperWriteConnection) enableTamper() {
	connection.mu.Lock()
	connection.tamper = true
	connection.mu.Unlock()
}

func (connection *tamperWriteConnection) Write(data []byte) (int, error) {
	connection.mu.Lock()
	shouldTamper := connection.tamper && !connection.tampered && len(data) > 4
	if shouldTamper {
		connection.tampered = true
	}
	connection.mu.Unlock()
	if !shouldTamper {
		return connection.Conn.Write(data)
	}
	mutated := append([]byte(nil), data...)
	mutated[len(mutated)-1] ^= 1
	written, err := connection.Conn.Write(mutated)
	for index := range mutated {
		mutated[index] = 0
	}
	return written, err
}

var _ net.Listener = (*oneConnectionListener)(nil)
var _ net.Conn = (*tamperWriteConnection)(nil)
