package runner

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"net"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/mdlayher/vsock"
)

type fakeControlManager struct {
	mu         sync.Mutex
	applyCalls int
	callCalls  int
	startCalls int
	blockApply chan struct{}
	blockCall  chan struct{}
}

func (manager *fakeControlManager) ApplyCatalog(ctx context.Context, _ []byte) error {
	manager.mu.Lock()
	manager.applyCalls++
	block := manager.blockApply
	manager.mu.Unlock()
	if block != nil {
		select {
		case <-block:
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	return nil
}

func (manager *fakeControlManager) Start(context.Context, string, json.RawMessage, []string, []Workspace) error {
	manager.mu.Lock()
	manager.startCalls++
	manager.mu.Unlock()
	return nil
}

func (*fakeControlManager) Stop(context.Context, string, string) error { return nil }

func (manager *fakeControlManager) Call(ctx context.Context, _ string, _ string, _ json.RawMessage) (json.RawMessage, error) {
	manager.mu.Lock()
	manager.callCalls++
	block := manager.blockCall
	manager.mu.Unlock()
	if block != nil {
		select {
		case <-block:
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	return json.RawMessage(`{"protocol":"grok.computer-use/v1","type":"action-result"}`), nil
}

func (*fakeControlManager) Statuses() (uint64, []IntegrationStatus) {
	return 7, []IntegrationStatus{}
}

type fakeWorkspaceMounter struct {
	mu    sync.Mutex
	calls []Workspace
}

func (mounter *fakeWorkspaceMounter) Prepare(_ context.Context, mountID, path string) error {
	mounter.mu.Lock()
	mounter.calls = append(mounter.calls, Workspace{MountID: mountID, Path: path, ReadOnly: true})
	mounter.mu.Unlock()
	return nil
}

func TestControlServerHealthAndStrictEnvelope(t *testing.T) {
	server, now := newTestControlServer(t, &fakeControlManager{})
	response := decodeControlResponse(t, server.Handle(context.Background(), controlRequestJSON(t, "health-1", "runner.health", `{ }`, now.Add(time.Minute))))
	if !response.OK || response.ID != "health-1" || response.Error != nil {
		t.Fatalf("unexpected health response: %+v", response)
	}
	var result struct {
		ImageVersion    string              `json:"imageVersion"`
		CatalogRevision uint64              `json:"catalogRevision"`
		Integrations    []IntegrationStatus `json:"integrations"`
	}
	if err := json.Unmarshal(response.Result, &result); err != nil || result.ImageVersion != "test-image" || result.CatalogRevision != 7 || result.Integrations == nil {
		t.Fatalf("unexpected health result: %s (%v)", response.Result, err)
	}

	duplicate := []byte(`{"protocol":"grok.guest-control/v1","type":"request","id":"one","id":"two","method":"runner.health","deadlineUnixMs":1,"params":{}}`)
	invalid := decodeControlResponse(t, server.Handle(context.Background(), duplicate))
	if invalid.OK || invalid.Error == nil || invalid.Error.Code != "INVALID_ARGUMENT" || invalid.ID != "invalid" {
		t.Fatalf("duplicate key was not rejected: %+v", invalid)
	}
}

func TestControlServerReplaysIdenticalRequestsAndRejectsIDReuse(t *testing.T) {
	manager := &fakeControlManager{}
	server, now := newTestControlServer(t, manager)
	request := controlRequestJSON(t, "call-1", "integration.call", `{"integrationId":"desktop.grok.wisp","method":"computer-use.observe","params":{}}`, now.Add(time.Minute))
	first := server.Handle(context.Background(), request)
	second := server.Handle(context.Background(), request)
	if string(first) != string(second) {
		t.Fatalf("replayed response changed:\n%s\n%s", first, second)
	}
	manager.mu.Lock()
	calls := manager.callCalls
	manager.mu.Unlock()
	if calls != 1 {
		t.Fatalf("manager was called %d times, want 1", calls)
	}

	conflicting := controlRequestJSON(t, "call-1", "runner.health", `{}`, now.Add(time.Minute))
	response := decodeControlResponse(t, server.Handle(context.Background(), conflicting))
	if response.OK || response.Error == nil || response.Error.Code != "ALREADY_EXISTS" {
		t.Fatalf("request ID reuse was not rejected: %+v", response)
	}
}

func TestControlServerCoalescesConcurrentRequests(t *testing.T) {
	manager := &fakeControlManager{blockApply: make(chan struct{})}
	server, now := newTestControlServer(t, manager)
	request := controlRequestJSON(t, "catalog-1", "catalog.apply", `{"catalog":{"version":1}}`, now.Add(time.Minute))
	responses := make(chan []byte, 2)
	go func() { responses <- server.Handle(context.Background(), request) }()

	deadline := time.Now().Add(time.Second)
	for {
		manager.mu.Lock()
		calls := manager.applyCalls
		manager.mu.Unlock()
		if calls == 1 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatal("first catalog request did not reach the manager")
		}
		time.Sleep(time.Millisecond)
	}
	go func() { responses <- server.Handle(context.Background(), request) }()
	close(manager.blockApply)
	first, second := <-responses, <-responses
	if string(first) != string(second) {
		t.Fatalf("coalesced responses differ:\n%s\n%s", first, second)
	}
	manager.mu.Lock()
	calls := manager.applyCalls
	manager.mu.Unlock()
	if calls != 1 {
		t.Fatalf("manager was called %d times, want 1", calls)
	}
}

func TestControlServerReservesReplayBytesBeforeDispatch(t *testing.T) {
	manager := &fakeControlManager{blockCall: make(chan struct{})}
	server, now := newTestControlServer(t, manager)
	server.policy.MaxMessageBytes = 16 << 20
	responses := make(chan []byte, 4)
	for index := 0; index < 4; index++ {
		id := fmt.Sprintf("call-%d", index)
		request := controlRequestJSON(t, id, "integration.call", `{"integrationId":"desktop.grok.wisp","method":"computer-use.act","params":{}}`, now.Add(time.Minute))
		go func() { responses <- server.Handle(context.Background(), request) }()
	}
	waitForControlCalls(t, manager, 4)
	overflow := controlRequestJSON(t, "call-overflow", "integration.call", `{"integrationId":"desktop.grok.wisp","method":"computer-use.act","params":{}}`, now.Add(time.Minute))
	response := decodeControlResponse(t, server.Handle(context.Background(), overflow))
	if response.OK || response.Error == nil || response.Error.Code != "RESOURCE_EXHAUSTED" {
		t.Fatalf("replay byte exhaustion did not fail closed: %+v", response)
	}
	close(manager.blockCall)
	for range 4 {
		<-responses
	}
	server.replayMu.Lock()
	bytes, inflight := server.replayBytes, server.replayInflight
	server.replayMu.Unlock()
	if bytes > maxReplayBytes || inflight != 0 {
		t.Fatalf("replay accounting is invalid: bytes=%d inflight=%d", bytes, inflight)
	}
}

func TestControlServerLimitsReplayInflightOwners(t *testing.T) {
	manager := &fakeControlManager{blockCall: make(chan struct{})}
	server, now := newTestControlServer(t, manager)
	server.policy.MaxMessageBytes = 4096
	responses := make(chan []byte, maxReplayInflight)
	params := `{"integrationId":"desktop.grok.wisp","method":"computer-use.observe","params":{}}`
	for index := 0; index < maxReplayInflight; index++ {
		request := controlRequestJSON(t, fmt.Sprintf("observe-%d", index), "integration.call", params, now.Add(time.Minute))
		go func() { responses <- server.Handle(context.Background(), request) }()
	}
	waitForControlCalls(t, manager, maxReplayInflight)
	overflow := decodeControlResponse(t, server.Handle(context.Background(), controlRequestJSON(t, "observe-overflow", "integration.call", params, now.Add(time.Minute))))
	if overflow.OK || overflow.Error == nil || overflow.Error.Code != "RESOURCE_EXHAUSTED" {
		t.Fatalf("in-flight replay limit did not fail closed: %+v", overflow)
	}
	close(manager.blockCall)
	for range maxReplayInflight {
		<-responses
	}
}

func TestControlServerPinsUnexpiredActionResponses(t *testing.T) {
	manager := &fakeControlManager{}
	server, now := newTestControlServer(t, manager)
	params := `{"integrationId":"desktop.grok.wisp","method":"computer-use.act","params":{}}`
	for index := 0; index < maxReplayEntries; index++ {
		id := fmt.Sprintf("action-%d", index)
		response := decodeControlResponse(t, server.Handle(context.Background(), controlRequestJSON(t, id, "integration.call", params, now.Add(time.Minute))))
		if !response.OK {
			t.Fatalf("action %d was rejected before capacity: %+v", index, response)
		}
	}
	overflow := decodeControlResponse(t, server.Handle(context.Background(), controlRequestJSON(t, "action-overflow", "integration.call", params, now.Add(time.Minute))))
	if overflow.OK || overflow.Error == nil || overflow.Error.Code != "RESOURCE_EXHAUSTED" {
		t.Fatalf("unexpired action response was evicted: %+v", overflow)
	}
}

func waitForControlCalls(t *testing.T, manager *fakeControlManager, expected int) {
	t.Helper()
	deadline := time.Now().Add(time.Second)
	for {
		manager.mu.Lock()
		calls := manager.callCalls
		manager.mu.Unlock()
		if calls == expected {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("manager received %d calls, want %d", calls, expected)
		}
		time.Sleep(time.Millisecond)
	}
}

func TestControlServerRejectsInvalidDeadline(t *testing.T) {
	server, now := newTestControlServer(t, &fakeControlManager{})
	for name, deadline := range map[string]time.Time{
		"expired":  now.Add(-time.Millisecond),
		"too long": now.Add(maxRequestLifetime + time.Second),
	} {
		t.Run(name, func(t *testing.T) {
			response := decodeControlResponse(t, server.Handle(context.Background(), controlRequestJSON(t, name, "runner.health", `{}`, deadline)))
			if response.OK || response.Error == nil || response.Error.Code != "INVALID_ARGUMENT" {
				t.Fatalf("deadline was not rejected: %+v", response)
			}
		})
	}
}

func TestControlServerPreparesOnlyValidatedWorkspaceMounts(t *testing.T) {
	manager := &fakeControlManager{}
	mounter := &fakeWorkspaceMounter{}
	server, now := newTestControlServer(t, manager)
	server.mounter = mounter
	root := server.policy.WorkspaceRoot
	params := `{"integrationId":"desktop.grok.wisp","config":{},"grants":[],"workspaces":[{"mountId":"project","path":"` + root + `/project","readOnly":true}]}`
	response := decodeControlResponse(t, server.Handle(context.Background(), controlRequestJSON(t, "start-1", "integration.start", params, now.Add(time.Minute))))
	if !response.OK {
		t.Fatalf("valid workspace start was rejected: %+v", response)
	}
	mounter.mu.Lock()
	mountCalls := append([]Workspace(nil), mounter.calls...)
	mounter.mu.Unlock()
	manager.mu.Lock()
	startCalls := manager.startCalls
	manager.mu.Unlock()
	if len(mountCalls) != 1 || mountCalls[0].MountID != "project" || startCalls != 1 {
		t.Fatalf("unexpected mount/start calls: mounts=%+v starts=%d", mountCalls, startCalls)
	}

	invalidParams := `{"integrationId":"desktop.grok.wisp","config":{},"grants":[],"workspaces":[{"mountId":"project","path":"` + root + `/other","readOnly":true}]}`
	invalid := decodeControlResponse(t, server.Handle(context.Background(), controlRequestJSON(t, "start-2", "integration.start", invalidParams, now.Add(time.Minute))))
	if invalid.OK || invalid.Error == nil || invalid.Error.Code != "INVALID_ARGUMENT" {
		t.Fatalf("mismatched workspace mount was accepted: %+v", invalid)
	}
}

func TestHostVSockAddressPolicy(t *testing.T) {
	if !isHostVSockAddress(&vsock.Addr{ContextID: vsock.Host, Port: defaultPort}) {
		t.Fatal("host CID was rejected")
	}
	for _, address := range []net.Addr{
		&vsock.Addr{ContextID: vsock.Hypervisor, Port: defaultPort},
		&vsock.Addr{ContextID: 42, Port: defaultPort},
		&net.TCPAddr{},
	} {
		if isHostVSockAddress(address) {
			t.Fatalf("non-host address was accepted: %v", address)
		}
	}
}

func TestBoundedLineRejectsOversizedFrames(t *testing.T) {
	_, err := readBoundedLine(bufio.NewReader(strings.NewReader(strings.Repeat("x", 65)+"\n")), 64)
	if err == nil {
		t.Fatal("oversized frame was accepted")
	}
}
func newTestControlServer(t *testing.T, manager controlManager) (*ControlServer, time.Time) {
	t.Helper()
	policy := Policy{
		Version: 1, ImageVersion: "test-image", ManifestRoots: []string{"/bundles"},
		WorkspaceRoot: "/workspaces", StateRoot: "/state", MaxMessageBytes: 1 << 20,
		ControlPort: defaultPort, BundleOwnerUID: 0, BubblewrapPath: "/bin/bwrap",
		ComputerUseSchema: "/schemas/computer-use.json", WorkspaceMounterSocket: "/run/mounter.sock",
		Transport: TransportPolicy{Family: "AF_VSOCK", Purpose: "control"},
	}
	server, err := NewControlServer(policy, manager)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Now().Round(time.Millisecond)
	server.now = func() time.Time { return now }
	return server, now
}

func controlRequestJSON(t *testing.T, id, method, params string, deadline time.Time) []byte {
	t.Helper()
	request := controlRequest{
		Protocol: controlProtocol, Type: "request", ID: id, Method: method,
		DeadlineUnixMS: deadline.UnixMilli(), Params: json.RawMessage(params),
	}
	data, err := json.Marshal(request)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func decodeControlResponse(t *testing.T, data []byte) controlResponse {
	t.Helper()
	var response controlResponse
	if err := json.Unmarshal(data, &response); err != nil {
		t.Fatalf("invalid response %q: %v", data, err)
	}
	return response
}
