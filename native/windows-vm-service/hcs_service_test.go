//go:build !windows

package vmservice

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/grok-insider/grok-desktop/native/windows-vm-service/internal/hcsapi"
)

const testRuntimeID = "11111111-1111-4111-8111-111111111111"

func TestHCSCapabilitiesMatchSharedContractFixture(t *testing.T) {
	service := newTestHCSService(t, makeTestRoots(t), newFakeHCS())
	capabilities, err := service.GetCapabilities(
		context.Background(),
		GetCapabilitiesRequest{Request: identity("capabilities-contract")},
	)
	if err != nil {
		t.Fatalf("GetCapabilities: %v", err)
	}
	actual, err := json.Marshal(capabilities)
	if err != nil {
		t.Fatalf("marshal capabilities: %v", err)
	}
	expected, err := os.ReadFile(filepath.Join("testdata", "capabilities-1.1.0.json"))
	if err != nil {
		t.Fatalf("read shared capabilities fixture: %v", err)
	}
	if !bytes.Equal(actual, bytes.TrimSpace(expected)) {
		t.Fatalf("capabilities contract drifted\nactual:   %s\nexpected: %s", actual, expected)
	}
}

type fakeHCS struct {
	mu          sync.Mutex
	probeErr    error
	systems     map[string]hcsapi.System
	documents   map[string][]byte
	grants      map[string]string
	createCalls int
	terminated  []string
	onCreate    func()
	owner       string
}

func newFakeHCS() *fakeHCS {
	return &fakeHCS{
		systems:   map[string]hcsapi.System{},
		documents: map[string][]byte{},
		grants:    map[string]string{},
	}
}

func (f *fakeHCS) Probe(context.Context) error { return f.probeErr }

func (f *fakeHCS) Enumerate(_ context.Context, owner string) ([]hcsapi.System, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	result := make([]hcsapi.System, 0, len(f.systems))
	for _, system := range f.systems {
		if system.Owner == owner {
			result = append(result, system)
		}
	}
	return result, nil
}

func (f *fakeHCS) Create(_ context.Context, id string, document []byte) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.onCreate != nil {
		f.onCreate()
	}
	if _, exists := f.systems[id]; exists {
		return &hcsapi.Error{Operation: "create", Code: 0x8037010f}
	}
	f.createCalls++
	f.documents[id] = append([]byte(nil), document...)
	f.systems[id] = hcsapi.System{ID: id, Owner: f.owner, State: "Created", Type: "VirtualMachine"}
	return nil
}

func (f *fakeHCS) Start(_ context.Context, id string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	system, exists := f.systems[id]
	if !exists {
		return &hcsapi.Error{Operation: "start", Code: 0x8037010e}
	}
	system.State = "Running"
	system.RuntimeID = testRuntimeID
	f.systems[id] = system
	return nil
}

func (f *fakeHCS) Shutdown(ctx context.Context, id string) error { return f.Terminate(ctx, id) }

func (f *fakeHCS) Terminate(_ context.Context, id string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	if _, exists := f.systems[id]; !exists {
		return &hcsapi.Error{Operation: "terminate", Code: 0x8037010e}
	}
	delete(f.systems, id)
	f.terminated = append(f.terminated, id)
	return nil
}

func (f *fakeHCS) GrantVMAccess(_ context.Context, id, path string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.grants[id] = path
	return nil
}

func (f *fakeHCS) RevokeVMAccess(_ context.Context, id, _ string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	delete(f.grants, id)
	return nil
}

type testRoots struct {
	config    Config
	image     string
	workspace string
}

func makeTestRoots(t *testing.T) testRoots {
	t.Helper()
	root := t.TempDir()
	image := filepath.Join(root, "images")
	workspace := filepath.Join(root, "workspaces")
	if err := os.MkdirAll(filepath.Join(image, "staging"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(workspace, 0o700); err != nil {
		t.Fatal(err)
	}
	return testRoots{
		config: Config{
			CurrentUserSID: "S-1-5-21-1000-1001-1002-1003", ImageRoot: image, WorkspaceRoot: workspace,
			GuestImagePolicy: testGuestImagePolicy("nixos", "nixos.vhdx", []byte("disk")),
		},
		image: image, workspace: workspace,
	}
}

func testGuestImagePolicy(id, stagingName string, content []byte) *GuestImagePolicy {
	digest := sha256.Sum256(content)
	return &GuestImagePolicy{
		architecture: "x64", sequence: 1, catalogSHA256: strings.Repeat("a", 64), signingKeyID: "test-release",
		images: map[string]OfficialGuestImage{id: {
			ID: id, Version: "1.0.0", StagingName: stagingName,
			SHA256: hex.EncodeToString(digest[:]), SizeBytes: int64(len(content)),
		}},
	}
}

func newTestHCSService(t *testing.T, roots testRoots, fake *fakeHCS) *hcsService {
	t.Helper()
	fake.owner = deriveOwner(roots.config.CurrentUserSID)
	dialer := newTestGuestDialer()
	service, err := newHCSServiceWithGuestDialer(
		context.Background(), roots.config, fake, newNativePathValidator(), dialer,
	)
	if err != nil {
		t.Fatalf("newHCSService: %v", err)
	}
	t.Cleanup(func() { _ = service.Close(context.Background()) })
	return service
}

func ensureHCSImage(t *testing.T, service *hcsService, roots testRoots, id string, content []byte) Image {
	t.Helper()
	path := filepath.Join(roots.image, "staging", id+".vhdx")
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(content)
	image, err := service.EnsureImage(context.Background(), EnsureImageRequest{
		Request: identity("ensure-" + id), ImageID: id, RelativePath: filepath.ToSlash(filepath.Join("staging", id+".vhdx")),
		SHA256: hex.EncodeToString(digest[:]), SizeBytes: int64(len(content)),
	})
	if err != nil {
		t.Fatalf("EnsureImage: %v", err)
	}
	return image
}

func createTestVM(t *testing.T, service *hcsService, id, imageID string) Vm {
	t.Helper()
	vm, err := service.CreateVm(context.Background(), CreateVmRequest{
		Request: identity("create-" + id), VmID: id, ImageID: imageID, VCPUCount: 2, MemoryMiB: 2048,
	})
	if err != nil {
		t.Fatalf("CreateVm: %v", err)
	}
	return vm
}

func TestHCSServiceLifecycleUsesFixedSecureDocument(t *testing.T) {
	roots := makeTestRoots(t)
	fake := newFakeHCS()
	service := newTestHCSService(t, roots, fake)
	ensureHCSImage(t, service, roots, "nixos", []byte("disk"))
	createTestVM(t, service, "work-vm", "nixos")
	workspacePath := filepath.Join(roots.workspace, "project")
	if err := os.Mkdir(workspacePath, 0o700); err != nil {
		t.Fatal(err)
	}
	attachment, err := service.AttachWorkspace(context.Background(), AttachWorkspaceRequest{
		Request: identity("attach"), VmID: "work-vm", MountID: "project", RelativePath: "project", ReadOnly: true,
	})
	if err != nil {
		t.Fatalf("AttachWorkspace: %v", err)
	}
	if !attachment.ReadOnly {
		t.Fatal("workspace attachment was not read-only")
	}
	fake.onCreate = func() {
		contents, readErr := os.ReadFile(filepath.Join(roots.image, ".vm-service", "state.json"))
		if readErr != nil {
			t.Errorf("read persisted start intent: %v", readErr)
			return
		}
		var state serviceState
		if decodeErr := json.Unmarshal(contents, &state); decodeErr != nil {
			t.Errorf("decode persisted start intent: %v", decodeErr)
			return
		}
		if state.VMs["work-vm"].PendingOperation != "starting" {
			t.Errorf("HCS Create ran before start intent was durable: %#v", state.VMs["work-vm"])
		}
	}
	running, err := service.StartVm(context.Background(), StartVmRequest{Request: identity("start"), VmID: "work-vm"})
	if err != nil {
		t.Fatalf("StartVm: %v", err)
	}
	if running.State != VmStateRunning {
		t.Fatalf("state = %q, want running", running.State)
	}

	stored := service.state.VMs["work-vm"]
	var document map[string]any
	if err := json.Unmarshal(fake.documents[stored.HCSID], &document); err != nil {
		t.Fatalf("decode HCS document: %v", err)
	}
	encoded := string(fake.documents[stored.HCSID])
	for _, forbidden := range []string{"NetworkAdapters", "CommandLine", "Process", "PowerShell"} {
		if strings.Contains(encoded, `"`+forbidden+`":`) {
			t.Fatalf("HCS document contains forbidden field %q: %s", forbidden, encoded)
		}
	}
	if strings.Contains(encoded, roots.config.CurrentUserSID) {
		t.Fatalf("HCS document contains the tenant SID: %s", encoded)
	}
	if strings.Contains(encoded, ";;;BA)") || strings.Contains(encoded, "S-1-5-32-544") {
		t.Fatalf("HCS document grants a socket right to Builtin Administrators: %s", encoded)
	}
	if document["Owner"] != service.owner {
		t.Fatalf("Owner = %#v", document["Owner"])
	}
	virtualMachine := document["VirtualMachine"].(map[string]any)
	devices := virtualMachine["Devices"].(map[string]any)
	shares := devices["Plan9"].(map[string]any)["Shares"].([]any)
	share := shares[0].(map[string]any)
	if share["Flags"] != float64(plan9ShareReadOnly|plan9ShareLinuxMetadata) || share["Port"] != float64(plan9Port) {
		t.Fatalf("Plan9 share is not host-enforced read-only: %#v", share)
	}
	hvSocket := devices["HvSocket"].(map[string]any)["HvSocketConfig"].(map[string]any)
	const expectedDenyAllDescriptor = "D:P(D;;GA;;;WD)"
	if hvSocket["DefaultBindSecurityDescriptor"] != expectedDenyAllDescriptor || hvSocket["DefaultConnectSecurityDescriptor"] != expectedDenyAllDescriptor {
		t.Fatalf("unlisted Hyper-V sockets are not denied: %#v", hvSocket)
	}
	services := hvSocket["ServiceTable"].(map[string]any)
	if len(services) != 2 || services[controlServiceID] == nil || services[computerUseServiceID] == nil {
		t.Fatalf("unexpected Hyper-V socket service table: %#v", services)
	}
	for id, raw := range services {
		config := raw.(map[string]any)
		if wildcard, present := config["AllowWildcardBinds"]; present && wildcard == true {
			t.Fatalf("service %s permits wildcard binds", id)
		}
		const expectedLocalSystemDescriptor = "D:P(A;;GA;;;SY)"
		if config["BindSecurityDescriptor"] != expectedLocalSystemDescriptor || config["ConnectSecurityDescriptor"] != expectedLocalSystemDescriptor {
			t.Fatalf("service %s is not restricted to LocalSystem: %#v", id, config)
		}
	}

	_, err = service.OpenSocket(context.Background(), OpenSocketRequest{
		Request: identity("socket"), VmID: "work-vm", Purpose: SocketPurposeControl,
	})
	assertServiceCode(t, err, CodeUnavailable)
	stopped, err := service.StopVm(context.Background(), StopVmRequest{
		Request: identity("stop"), VmID: "work-vm", Mode: StopModeGraceful,
	})
	if err != nil || stopped.State != VmStateStopped {
		t.Fatalf("StopVm = %#v, %v", stopped, err)
	}
	if err := service.DeleteVm(context.Background(), DeleteVmRequest{Request: identity("delete"), VmID: "work-vm"}); err != nil {
		t.Fatalf("DeleteVm: %v", err)
	}
	if _, err := os.Stat(filepath.Join(roots.image, ".vm-service", "vms", "work-vm")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("VM directory remains after delete: %v", err)
	}
}

func TestEnsureImageMismatchIsNotInstalled(t *testing.T) {
	roots := makeTestRoots(t)
	service := newTestHCSService(t, roots, newFakeHCS())
	content := []byte("disk")
	if err := os.WriteFile(filepath.Join(roots.image, "staging", "nixos.vhdx"), content, 0o600); err != nil {
		t.Fatal(err)
	}
	_, err := service.EnsureImage(context.Background(), EnsureImageRequest{
		Request: identity("bad-digest"), ImageID: "nixos", RelativePath: "staging/nixos.vhdx",
		SHA256: strings.Repeat("0", 64), SizeBytes: int64(len(content)),
	})
	assertServiceCode(t, err, CodeInvalidArgument)
	digest := sha256.Sum256(content)
	_, err = service.EnsureImage(context.Background(), EnsureImageRequest{
		Request: identity("bad-size"), ImageID: "nixos", RelativePath: "staging/nixos.vhdx",
		SHA256: hex.EncodeToString(digest[:]), SizeBytes: int64(len(content) + 1),
	})
	assertServiceCode(t, err, CodeInvalidArgument)
	_, err = service.EnsureImage(context.Background(), EnsureImageRequest{
		Request: identity("unsigned-image"), ImageID: "other", RelativePath: "staging/nixos.vhdx",
	})
	assertServiceCode(t, err, CodePermissionDenied)
	if _, exists := service.state.Images["nixos"]; exists {
		t.Fatal("mismatched image entered metadata")
	}
	if _, err := os.Stat(filepath.Join(roots.image, ".vm-service", "installed", "nixos", "disk.vhdx")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("mismatched image was installed: %v", err)
	}
	image, err := service.EnsureImage(context.Background(), EnsureImageRequest{
		Request: identity("catalog-authoritative"), ImageID: "nixos", RelativePath: "staging/nixos.vhdx",
	})
	if err != nil {
		t.Fatalf("EnsureImage without caller metadata: %v", err)
	}
	if image.SHA256 != hex.EncodeToString(digest[:]) || image.SizeBytes != int64(len(content)) {
		t.Fatalf("installed metadata did not come from signed policy: %#v", image)
	}
}

func TestWorkspaceIdentityReplacementFailsClosedBeforeHCSCreate(t *testing.T) {
	roots := makeTestRoots(t)
	fake := newFakeHCS()
	service := newTestHCSService(t, roots, fake)
	ensureHCSImage(t, service, roots, "nixos", []byte("disk"))
	createTestVM(t, service, "secure-vm", "nixos")
	workspace := filepath.Join(roots.workspace, "project")
	if err := os.Mkdir(workspace, 0o700); err != nil {
		t.Fatal(err)
	}
	if _, err := service.AttachWorkspace(context.Background(), AttachWorkspaceRequest{
		Request: identity("attach"), VmID: "secure-vm", MountID: "project", RelativePath: "project", ReadOnly: true,
	}); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(workspace, workspace+"-old"); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(workspace, 0o700); err != nil {
		t.Fatal(err)
	}
	_, err := service.StartVm(context.Background(), StartVmRequest{Request: identity("start"), VmID: "secure-vm"})
	assertServiceCode(t, err, CodePermissionDenied)
	if fake.createCalls != 0 {
		t.Fatalf("HCS Create called %d times after identity replacement", fake.createCalls)
	}
}

func TestStartRejectsImageMetadataOutsideSignedCatalog(t *testing.T) {
	roots := makeTestRoots(t)
	fake := newFakeHCS()
	service := newTestHCSService(t, roots, fake)
	ensureHCSImage(t, service, roots, "nixos", []byte("disk"))
	createTestVM(t, service, "policy-vm", "nixos")
	stored := service.state.Images["nixos"]
	stored.SHA256 = strings.Repeat("0", 64)
	service.state.Images["nixos"] = stored

	_, err := service.StartVm(context.Background(), StartVmRequest{Request: identity("start-policy-vm"), VmID: "policy-vm"})
	assertServiceCode(t, err, CodePermissionDenied)
	if fake.createCalls != 0 {
		t.Fatalf("HCS Create called %d times with untrusted image metadata", fake.createCalls)
	}
}

func TestRestartReconciliationAdoptsKnownAndTerminatesOrphan(t *testing.T) {
	roots := makeTestRoots(t)
	fake := newFakeHCS()
	service := newTestHCSService(t, roots, fake)
	ensureHCSImage(t, service, roots, "nixos", []byte("disk"))
	createTestVM(t, service, "known-vm", "nixos")
	if _, err := service.StartVm(context.Background(), StartVmRequest{Request: identity("start"), VmID: "known-vm"}); err != nil {
		t.Fatal(err)
	}
	knownHCSID := service.state.VMs["known-vm"].HCSID
	orphanID := "22222222-2222-4222-8222-222222222222"
	fake.systems[orphanID] = hcsapi.System{
		ID: orphanID, Owner: service.owner, Type: "VirtualMachine", State: "Running", RuntimeID: testRuntimeID,
	}
	restarted := newTestHCSService(t, roots, fake)
	if restarted.state.VMs["known-vm"].State != VmStateRunning {
		t.Fatal("known running VM was not adopted")
	}
	if _, exists := fake.systems[orphanID]; exists {
		t.Fatal("owned orphan HCS VM was not terminated")
	}
	delete(fake.systems, knownHCSID)
	restarted = newTestHCSService(t, roots, fake)
	if restarted.state.VMs["known-vm"].State != VmStateStopped || restarted.state.VMs["known-vm"].RuntimeID != "" {
		t.Fatalf("missing runtime was not reconciled to stopped: %#v", restarted.state.VMs["known-vm"])
	}
}

func TestCorruptMetadataMakesBackendUnavailable(t *testing.T) {
	roots := makeTestRoots(t)
	fake := newFakeHCS()
	_ = newTestHCSService(t, roots, fake)
	statePath := filepath.Join(roots.image, ".vm-service", "state.json")
	if err := os.WriteFile(statePath, []byte(`{"version":1,"images":`), 0o600); err != nil {
		t.Fatal(err)
	}
	_, err := newHCSService(context.Background(), roots.config, fake, newNativePathValidator())
	assertServiceCode(t, err, CodeUnavailable)
}

func TestHCSProbeFailureMakesBackendUnavailable(t *testing.T) {
	roots := makeTestRoots(t)
	fake := newFakeHCS()
	fake.probeErr = &hcsapi.Error{Operation: "probe", Code: 0x80370102}
	_, err := newHCSService(context.Background(), roots.config, fake, newNativePathValidator())
	assertServiceCode(t, err, CodeUnavailable)
}

func assertServiceCode(t *testing.T, err error, code ErrorCode) {
	t.Helper()
	var serviceErr *Error
	if !errors.As(err, &serviceErr) || serviceErr.Code != code {
		t.Fatalf("error = %v, want service code %q", err, code)
	}
}
