//go:build !windows

package host

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	vmservice "github.com/grok-insider/grok-desktop/native/windows-vm-service"
	"github.com/grok-insider/grok-desktop/native/windows-vm-service/transport"
)

const hostTestSID = "S-1-5-21-1000-1001-1002-1003"

type countingService struct {
	vmservice.Service
	ensureCalls       atomic.Int32
	guestControlCalls atomic.Int32
	started           chan struct{}
	release           chan struct{}
}

func (s *countingService) GuestControl(_ context.Context, request vmservice.GuestControlRequest) (vmservice.GuestControlResult, error) {
	s.guestControlCalls.Add(1)
	response, _ := json.Marshal(map[string]any{
		"protocol": "grok.guest-control/v1",
		"type":     "response",
		"id":       request.OperationID,
		"ok":       true,
		"result":   map[string]any{"ready": true},
	})
	return vmservice.GuestControlResult{Response: response}, nil
}

func (s *countingService) EnsureImage(ctx context.Context, request vmservice.EnsureImageRequest) (vmservice.Image, error) {
	s.ensureCalls.Add(1)
	if s.started != nil {
		select {
		case s.started <- struct{}{}:
		default:
		}
	}
	if s.release != nil {
		select {
		case <-s.release:
		case <-ctx.Done():
			return vmservice.Image{}, ctx.Err()
		}
	}
	return s.Service.EnsureImage(ctx, request)
}

func TestHandleValidatesIdentityDeadlineAndPayload(t *testing.T) {
	now := time.Now().UTC()
	server, _ := newHostTestServer(t, now, nil)

	response := server.Handle(context.Background(), transport.PeerIdentity{UserSID: hostTestSID}, requestFrame(t, RequestEnvelope{
		Version: EnvelopeVersion, ID: "caps-1", Operation: vmservice.OperationGetCapabilities,
		Deadline: now.Add(time.Second).Format(time.RFC3339Nano), Payload: json.RawMessage(`{}`),
	}))
	if !response.OK {
		t.Fatalf("GetCapabilities failed: %#v", response.Error)
	}

	cases := []struct {
		name string
		edit func(*RequestEnvelope)
		code string
	}{
		{
			name: "expired deadline",
			edit: func(request *RequestEnvelope) { request.Deadline = now.Add(-time.Second).Format(time.RFC3339Nano) },
			code: errorDeadlineExceeded,
		},
		{
			name: "deadline too far",
			edit: func(request *RequestEnvelope) { request.Deadline = now.Add(time.Hour).Format(time.RFC3339Nano) },
			code: errorDeadlineTooFar,
		},
		{
			name: "caller identity field",
			edit: func(request *RequestEnvelope) {
				request.Payload = json.RawMessage(`{"request":{"userSid":"S-1-5-18"}}`)
			},
			code: errorMalformedRequest,
		},
		{
			name: "unknown operation",
			edit: func(request *RequestEnvelope) { request.Operation = "execute" },
			code: errorUnknownOperation,
		},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			request := RequestEnvelope{
				Version: EnvelopeVersion, ID: "request-1", Operation: vmservice.OperationGetCapabilities,
				Deadline: now.Add(time.Second).Format(time.RFC3339Nano), Payload: json.RawMessage(`{}`),
			}
			test.edit(&request)
			response := server.Handle(context.Background(), transport.PeerIdentity{UserSID: hostTestSID}, requestFrame(t, request))
			assertResponseCode(t, response, test.code)
		})
	}
}

func TestHandleRequiresAndReplaysIdempotencyKey(t *testing.T) {
	now := time.Now().UTC()
	server, service := newHostTestServer(t, now, nil)
	request := RequestEnvelope{
		Version: EnvelopeVersion, ID: "ensure-1", Operation: vmservice.OperationEnsureImage,
		Deadline: now.Add(time.Second).Format(time.RFC3339Nano),
		Payload: requestPayload(t, EnsureImagePayload{
			ImageID: "guest-v1", RelativePath: "images/guest.vhdx",
			SHA256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", SizeBytes: 4096,
		}),
	}
	response := server.Handle(context.Background(), transport.PeerIdentity{UserSID: hostTestSID}, requestFrame(t, request))
	assertResponseCode(t, response, errorIdempotencyRequired)

	request.IdempotencyKey = "ensure-key-0000001"
	response = server.Handle(context.Background(), transport.PeerIdentity{UserSID: hostTestSID}, requestFrame(t, request))
	if !response.OK {
		t.Fatalf("first EnsureImage failed: %#v", response.Error)
	}
	request.ID = "ensure-2"
	request.Deadline = now.Add(2 * time.Second).Format(time.RFC3339Nano)
	replayed := server.Handle(context.Background(), transport.PeerIdentity{UserSID: hostTestSID}, requestFrame(t, request))
	if !replayed.OK || replayed.ID != "ensure-2" {
		t.Fatalf("replayed response = %#v", replayed)
	}
	if calls := service.ensureCalls.Load(); calls != 1 {
		t.Fatalf("EnsureImage calls = %d, want 1", calls)
	}

	request.ID = "ensure-3"
	request.Payload = requestPayload(t, EnsureImagePayload{
		ImageID: "guest-v1", RelativePath: "images/other.vhdx",
		SHA256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", SizeBytes: 4096,
	})
	conflict := server.Handle(context.Background(), transport.PeerIdentity{UserSID: hostTestSID}, requestFrame(t, request))
	assertResponseCode(t, conflict, errorIdempotencyConflict)
}

func TestHandleCoalescesConcurrentIdempotentRequests(t *testing.T) {
	now := time.Now().UTC()
	blocking := &countingService{started: make(chan struct{}, 1), release: make(chan struct{})}
	server, service := newHostTestServer(t, now, blocking)
	request := RequestEnvelope{
		Version: EnvelopeVersion, ID: "ensure-1", Operation: vmservice.OperationEnsureImage,
		Deadline: now.Add(5 * time.Second).Format(time.RFC3339Nano), IdempotencyKey: "ensure-key-0000001",
		Payload: requestPayload(t, EnsureImagePayload{
			ImageID: "guest-v1", RelativePath: "images/guest.vhdx",
			SHA256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", SizeBytes: 4096,
		}),
	}
	responses := make(chan ResponseEnvelope, 2)
	firstFrame := requestFrame(t, request)
	go func() {
		responses <- server.Handle(context.Background(), transport.PeerIdentity{UserSID: hostTestSID}, firstFrame)
	}()
	select {
	case <-service.started:
	case <-time.After(time.Second):
		t.Fatal("first request did not reach service")
	}
	request.ID = "ensure-2"
	secondFrame := requestFrame(t, request)
	go func() {
		responses <- server.Handle(context.Background(), transport.PeerIdentity{UserSID: hostTestSID}, secondFrame)
	}()
	close(service.release)
	for range 2 {
		if response := <-responses; !response.OK {
			t.Fatalf("concurrent response failed: %#v", response.Error)
		}
	}
	if calls := service.ensureCalls.Load(); calls != 1 {
		t.Fatalf("EnsureImage calls = %d, want 1", calls)
	}
}

func TestGuestControlRequiresQualifiedDesktopProcess(t *testing.T) {
	now := time.Now().UTC()
	server, service := newHostTestServer(t, now, nil)
	request := RequestEnvelope{
		Version: EnvelopeVersion, ID: "guest-control-1", Operation: vmservice.OperationGuestControl,
		Deadline: now.Add(time.Second).Format(time.RFC3339Nano), IdempotencyKey: "guest-control-key-0001",
		Payload: requestPayload(t, GuestControlPayload{
			VmID: "work-vm", Method: vmservice.GuestControlRunnerHealth, Params: json.RawMessage(`{}`),
		}),
	}
	frame := requestFrame(t, request)
	response := server.Handle(context.Background(), transport.PeerIdentity{UserSID: hostTestSID}, frame)
	assertResponseCode(t, response, string(vmservice.CodePermissionDenied))
	if calls := service.guestControlCalls.Load(); calls != 0 {
		t.Fatalf("unqualified guest control calls = %d, want zero", calls)
	}

	qualified := transport.PeerIdentity{UserSID: hostTestSID, GuestControlQualified: true}
	response = server.Handle(context.Background(), qualified, frame)
	if !response.OK {
		t.Fatalf("qualified GuestControl failed: %#v", response.Error)
	}
	if calls := service.guestControlCalls.Load(); calls != 1 {
		t.Fatalf("qualified guest control calls = %d, want one", calls)
	}
	var result vmservice.GuestControlResult
	if err := json.Unmarshal(response.Result, &result); err != nil || len(result.Response) == 0 {
		t.Fatalf("GuestControl result is invalid: %s (%v)", response.Result, err)
	}
}

func TestServeMemoryTransportAndGracefulShutdown(t *testing.T) {
	now := time.Now().UTC()
	server, _ := newHostTestServer(t, now, nil)
	listener := transport.NewMemoryListener(1)
	ctx, cancel := context.WithCancel(context.Background())
	serveResult := make(chan error, 1)
	go func() { serveResult <- server.Serve(ctx, listener) }()

	client, err := listener.DialContext(context.Background(), transport.PeerIdentity{UserSID: hostTestSID})
	if err != nil {
		t.Fatalf("DialContext: %v", err)
	}
	request := RequestEnvelope{
		Version: EnvelopeVersion, ID: "caps-1", Operation: vmservice.OperationGetCapabilities,
		Deadline: now.Add(time.Second).Format(time.RFC3339Nano), Payload: json.RawMessage(`{}`),
	}
	if _, err := client.Write(append(requestFrame(t, request), '\n')); err != nil {
		t.Fatalf("write request: %v", err)
	}
	line, err := bufio.NewReader(client).ReadBytes('\n')
	if err != nil {
		t.Fatalf("read response: %v", err)
	}
	var response ResponseEnvelope
	if err := json.Unmarshal(line, &response); err != nil {
		t.Fatalf("decode response: %v", err)
	}
	if !response.OK {
		t.Fatalf("integration response failed: %#v", response.Error)
	}

	cancel()
	select {
	case err := <-serveResult:
		if err != nil {
			t.Fatalf("Serve shutdown: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Serve did not shut down gracefully")
	}
	_ = client.Close()
}

func newHostTestServer(t *testing.T, now time.Time, wrapper *countingService) (*Server, *countingService) {
	t.Helper()
	root := t.TempDir()
	service, err := vmservice.NewStubService(vmservice.Config{
		CurrentUserSID: hostTestSID,
		ImageRoot:      filepath.Join(root, "images"), WorkspaceRoot: filepath.Join(root, "workspaces"),
	})
	if err != nil {
		t.Fatalf("NewStubService: %v", err)
	}
	if wrapper == nil {
		wrapper = &countingService{}
	}
	wrapper.Service = service
	server, err := New(Config{
		Service:            wrapper,
		Logger:             slog.New(slog.NewTextHandler(io.Discard, nil)),
		MaxMessageBytes:    4096,
		MaxRequestDeadline: 10 * time.Second,
		IdleTimeout:        time.Second,
		WriteTimeout:       time.Second,
		ShutdownTimeout:    time.Second,
		Now:                func() time.Time { return now },
	})
	if err != nil {
		t.Fatalf("New server: %v", err)
	}
	return server, wrapper
}

func requestPayload(t *testing.T, payload any) json.RawMessage {
	t.Helper()
	encoded, err := json.Marshal(payload)
	if err != nil {
		t.Fatalf("marshal payload: %v", err)
	}
	return encoded
}

func requestFrame(t *testing.T, request RequestEnvelope) []byte {
	t.Helper()
	encoded, err := json.Marshal(request)
	if err != nil {
		t.Fatalf("marshal request: %v", err)
	}
	return encoded
}

func assertResponseCode(t *testing.T, response ResponseEnvelope, code string) {
	t.Helper()
	if response.OK || response.Error == nil {
		t.Fatalf("response = %#v, want error %q", response, code)
	}
	if response.Error.Code != code {
		t.Fatalf("error code = %q, want %q", response.Error.Code, code)
	}
}
