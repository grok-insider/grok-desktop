//go:build !windows

package vmservice

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"sync"
	"testing"
	"time"

	guestchannelv2 "github.com/grok-insider/grok-desktop/native/windows-vm-service/guestchannel/v2"
)

type testGuestDialer struct {
	mu          sync.Mutex
	dials       int
	runtimeIDs  []string
	purposes    []SocketPurpose
	invalidNext bool
	requests    chan guestControlWireRequest
}

type failingGuestDialer struct{}

func (failingGuestDialer) Dial(context.Context, string, SocketPurpose) (net.Conn, error) {
	return nil, errors.New("guest listener is unavailable")
}

func newTestGuestDialer() *testGuestDialer {
	return &testGuestDialer{requests: make(chan guestControlWireRequest, 16)}
}

func (dialer *testGuestDialer) Dial(ctx context.Context, runtimeID string, purpose SocketPurpose) (net.Conn, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	host, guest := net.Pipe()
	dialer.mu.Lock()
	dialer.dials++
	dialer.runtimeIDs = append(dialer.runtimeIDs, runtimeID)
	dialer.purposes = append(dialer.purposes, purpose)
	dialer.mu.Unlock()
	go dialer.serve(guest)
	return host, nil
}

func (dialer *testGuestDialer) serve(connection net.Conn) {
	channel, err := guestchannelv2.AcceptGuest(
		context.Background(), connection, DefaultGuestControlMaxBytes, time.Second,
	)
	if err != nil {
		_ = connection.Close()
		return
	}
	defer channel.Close()
	for {
		request, err := channel.Receive(context.Background())
		if err != nil {
			return
		}
		var control guestControlWireRequest
		if err := json.Unmarshal(request.ControlFrame(), &control); err != nil {
			return
		}
		dialer.requests <- control

		dialer.mu.Lock()
		invalid := dialer.invalidNext
		dialer.invalidNext = false
		dialer.mu.Unlock()
		response := []byte(`{`)
		if !invalid {
			response, _ = json.Marshal(guestControlWireResponse{
				Protocol: guestControlProtocol, Type: "response", ID: control.ID, OK: true,
				Result: json.RawMessage(`{"accepted":true}`),
			})
		}
		if err := channel.Respond(context.Background(), request, response); err != nil {
			return
		}
	}
}

func (dialer *testGuestDialer) count() int {
	dialer.mu.Lock()
	defer dialer.mu.Unlock()
	return dialer.dials
}

func (dialer *testGuestDialer) invalidateNextResponse() {
	dialer.mu.Lock()
	dialer.invalidNext = true
	dialer.mu.Unlock()
}

func TestGuestControlOwnsChannelMetadataAndRekeysAfterVMRestart(t *testing.T) {
	service, dialer := runningGuestProxyTestService(t)

	first := callGuestControl(t, service, "guest-operation-0001", GuestControlRunnerHealth)
	if string(first.Response) != `{"protocol":"grok.guest-control/v1","type":"response","id":"guest-operation-0001","ok":true,"result":{"accepted":true}}` {
		t.Fatalf("unexpected guest response: %s", first.Response)
	}
	received := <-dialer.requests
	if received.ID != "guest-operation-0001" || received.Method != string(GuestControlRunnerHealth) ||
		received.Protocol != guestControlProtocol || received.Type != "request" || received.DeadlineUnixMS <= time.Now().UnixMilli() {
		t.Fatalf("service built invalid authenticated metadata: %#v", received)
	}
	callGuestControl(t, service, "guest-operation-0002", GuestControlRunnerHealth)
	if dials := dialer.count(); dials != 1 {
		t.Fatalf("channel dials = %d, want one reused channel", dials)
	}

	if _, err := service.StopVm(context.Background(), StopVmRequest{
		Request: identity("stop-for-rekey"), VmID: "work-vm", Mode: StopModeGraceful,
	}); err != nil {
		t.Fatalf("StopVm: %v", err)
	}
	if _, err := service.StartVm(context.Background(), StartVmRequest{
		Request: identity("start-for-rekey"), VmID: "work-vm",
	}); err != nil {
		t.Fatalf("StartVm: %v", err)
	}
	callGuestControl(t, service, "guest-operation-0003", GuestControlRunnerHealth)
	if dials := dialer.count(); dials != 2 {
		t.Fatalf("channel dials = %d, want a fresh channel after restart", dials)
	}
	for index, runtimeID := range dialer.runtimeIDs {
		if runtimeID != testRuntimeID || dialer.purposes[index] != SocketPurposeControl {
			t.Fatalf("dial %d targeted %q/%q", index, runtimeID, dialer.purposes[index])
		}
	}
}

func TestGuestControlPoisonsSemanticallyInvalidGuestResponse(t *testing.T) {
	service, dialer := runningGuestProxyTestService(t)
	dialer.invalidateNextResponse()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	_, err := service.GuestControl(ctx, GuestControlRequest{
		Request: identity("invalid-guest-response"), VmID: "work-vm",
		OperationID: "guest-operation-0010", Method: GuestControlRunnerHealth,
		Params: json.RawMessage(`{}`),
	})
	cancel()
	assertServiceCode(t, err, CodeUnavailable)

	callGuestControl(t, service, "guest-operation-0011", GuestControlRunnerHealth)
	if dials := dialer.count(); dials != 2 {
		t.Fatalf("channel dials = %d, want invalid response to force re-provisioning", dials)
	}
}

func TestGuestControlFailsClosedForInvalidInputAndClosedTenant(t *testing.T) {
	service, dialer := runningGuestProxyTestService(t)
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	_, err := service.GuestControl(ctx, GuestControlRequest{
		Request: identity("invalid-method"), VmID: "work-vm", OperationID: "guest-operation-0020",
		Method: "shell.execute", Params: json.RawMessage(`{}`),
	})
	cancel()
	assertServiceCode(t, err, CodeInvalidArgument)
	if dialer.count() != 1 {
		t.Fatal("invalid method reached the guest dialer")
	}
	if err := service.Close(context.Background()); err != nil {
		t.Fatalf("Close: %v", err)
	}
	ctx, cancel = context.WithTimeout(context.Background(), 2*time.Second)
	_, err = service.GuestControl(ctx, GuestControlRequest{
		Request: identity("closed-tenant"), VmID: "work-vm", OperationID: "guest-operation-0021",
		Method: GuestControlRunnerHealth, Params: json.RawMessage(`{}`),
	})
	cancel()
	assertServiceCode(t, err, CodeUnavailable)
}

func TestServiceRestartStopsRunningVMWhenGuestRekeyFails(t *testing.T) {
	roots := makeTestRoots(t)
	fake := newFakeHCS()
	service := newTestHCSService(t, roots, fake)
	ensureHCSImage(t, service, roots, "nixos", []byte("disk"))
	createTestVM(t, service, "work-vm", "nixos")
	if _, err := service.StartVm(context.Background(), StartVmRequest{
		Request: identity("start-before-service-restart"), VmID: "work-vm",
	}); err != nil {
		t.Fatalf("StartVm: %v", err)
	}
	service.channels.close()

	restarted, err := newHCSServiceWithGuestDialer(
		context.Background(), roots.config, fake, newNativePathValidator(), failingGuestDialer{},
	)
	if err != nil {
		t.Fatalf("restart recovery: %v", err)
	}
	t.Cleanup(func() { _ = restarted.Close(context.Background()) })
	vm := restarted.state.VMs["work-vm"]
	if vm.State != VmStateStopped || vm.RuntimeID != "" || vm.PendingOperation != "" {
		t.Fatalf("failed rekey retained an unauthenticated running VM: %#v", vm)
	}
	if len(fake.systems) != 0 {
		t.Fatalf("failed rekey left HCS runtime active: %#v", fake.systems)
	}
}

func runningGuestProxyTestService(t *testing.T) (*hcsService, *testGuestDialer) {
	t.Helper()
	roots := makeTestRoots(t)
	service := newTestHCSService(t, roots, newFakeHCS())
	dialer := newTestGuestDialer()
	service.channels.close()
	service.channels = newGuestChannelPool(dialer, service.config.guestControlMaxBytes)
	t.Cleanup(func() { _ = service.Close(context.Background()) })
	ensureHCSImage(t, service, roots, "nixos", []byte("disk"))
	createTestVM(t, service, "work-vm", "nixos")
	if _, err := service.StartVm(context.Background(), StartVmRequest{
		Request: identity("start-guest-proxy"), VmID: "work-vm",
	}); err != nil {
		t.Fatalf("StartVm: %v", err)
	}
	return service, dialer
}

func callGuestControl(t *testing.T, service *hcsService, operationID string, method GuestControlMethod) GuestControlResult {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	result, err := service.GuestControl(ctx, GuestControlRequest{
		Request: identity("call-" + operationID), VmID: "work-vm", OperationID: operationID,
		Method: method, Params: json.RawMessage(`{}`),
	})
	if err != nil {
		t.Fatalf("GuestControl: %v", err)
	}
	return result
}
