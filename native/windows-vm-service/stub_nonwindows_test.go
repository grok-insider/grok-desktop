//go:build !windows

package vmservice

import (
	"context"
	"errors"
	"path/filepath"
	"testing"
)

const testSID = "S-1-5-21-1000-1001-1002-1003"

func TestStubLifecycleAndSocket(t *testing.T) {
	service := newTestService(t)
	ctx := context.Background()

	capabilities, err := service.GetCapabilities(ctx, GetCapabilitiesRequest{Request: identity("caps")})
	if err != nil {
		t.Fatalf("GetCapabilities: %v", err)
	}
	if !capabilities.Simulated || capabilities.WorkspaceMode != "read-only" {
		t.Fatalf("unexpected capabilities: %#v", capabilities)
	}

	image, err := service.EnsureImage(ctx, EnsureImageRequest{
		Request: identity("image"), ImageID: "guest-v1", RelativePath: "images/guest.vhdx",
		SHA256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", SizeBytes: 4096,
	})
	if err != nil {
		t.Fatalf("EnsureImage: %v", err)
	}
	if image.RelativePath != "images/guest.vhdx" {
		t.Fatalf("unexpected image path: %q", image.RelativePath)
	}

	vm, err := service.CreateVm(ctx, CreateVmRequest{
		Request: identity("create"), VmID: "desktop", ImageID: image.ID, VCPUCount: 2, MemoryMiB: 2048,
	})
	if err != nil {
		t.Fatalf("CreateVm: %v", err)
	}
	if vm.State != VmStateCreated {
		t.Fatalf("CreateVm state = %q", vm.State)
	}

	attachment, err := service.AttachWorkspace(ctx, AttachWorkspaceRequest{
		Request: identity("attach"), VmID: vm.ID, MountID: "source", RelativePath: "projects/demo", ReadOnly: true,
	})
	if err != nil {
		t.Fatalf("AttachWorkspace: %v", err)
	}
	if !attachment.ReadOnly {
		t.Fatal("workspace attachment is writable")
	}

	vm, err = service.StartVm(ctx, StartVmRequest{Request: identity("start"), VmID: vm.ID})
	if err != nil {
		t.Fatalf("StartVm: %v", err)
	}
	if vm.State != VmStateRunning {
		t.Fatalf("StartVm state = %q", vm.State)
	}

	socket, err := service.OpenSocket(ctx, OpenSocketRequest{
		Request: identity("socket"), VmID: vm.ID, Purpose: SocketPurposeComputerUseV1,
	})
	if err != nil {
		t.Fatalf("OpenSocket: %v", err)
	}
	if socket.Endpoint == "" || socket.Purpose != SocketPurposeComputerUseV1 {
		t.Fatalf("unexpected socket: %#v", socket)
	}

	vm, err = service.StopVm(ctx, StopVmRequest{Request: identity("stop"), VmID: vm.ID})
	if err != nil {
		t.Fatalf("StopVm: %v", err)
	}
	if vm.State != VmStateStopped {
		t.Fatalf("StopVm state = %q", vm.State)
	}
	if err := service.DeleteVm(ctx, DeleteVmRequest{Request: identity("delete"), VmID: vm.ID}); err != nil {
		t.Fatalf("DeleteVm: %v", err)
	}
}

func TestStubRejectsInvalidTransitions(t *testing.T) {
	service := newTestService(t)
	ctx := context.Background()
	ensureTestImage(t, service)

	vm, err := service.CreateVm(ctx, CreateVmRequest{
		Request: identity("create"), VmID: "desktop", ImageID: "guest-v1", VCPUCount: 2, MemoryMiB: 2048,
	})
	if err != nil {
		t.Fatalf("CreateVm: %v", err)
	}

	assertCode(t, service.DeleteVm(ctx, DeleteVmRequest{Request: identity("delete-created"), VmID: "missing"}), CodeNotFound)
	_, err = service.StopVm(ctx, StopVmRequest{Request: identity("stop-created"), VmID: vm.ID})
	assertCode(t, err, CodeConflict)

	_, err = service.StartVm(ctx, StartVmRequest{Request: identity("start"), VmID: vm.ID})
	if err != nil {
		t.Fatalf("StartVm: %v", err)
	}
	_, err = service.StartVm(ctx, StartVmRequest{Request: identity("start-again"), VmID: vm.ID})
	assertCode(t, err, CodeConflict)
	assertCode(t, service.DeleteVm(ctx, DeleteVmRequest{Request: identity("delete-running"), VmID: vm.ID}), CodeConflict)

	_, err = service.AttachWorkspace(ctx, AttachWorkspaceRequest{
		Request: identity("attach-running"), VmID: vm.ID, MountID: "source", RelativePath: "projects/demo", ReadOnly: true,
	})
	assertCode(t, err, CodeConflict)
}

func TestStubRejectsIdentityAndUnsafeWorkspacePaths(t *testing.T) {
	service := newTestService(t)
	ctx := context.Background()
	ensureTestImage(t, service)
	_, err := service.CreateVm(ctx, CreateVmRequest{
		Request: identity("create"), VmID: "desktop", ImageID: "guest-v1", VCPUCount: 2, MemoryMiB: 2048,
	})
	if err != nil {
		t.Fatalf("CreateVm: %v", err)
	}

	_, err = service.GetCapabilities(ctx, GetCapabilitiesRequest{Request: RequestIdentity{
		RequestID: "wrong-user", UserSID: "S-1-5-21-999-999-999-999",
	}})
	assertCode(t, err, CodePermissionDenied)

	cases := []string{
		"../secrets", `..\secrets`, "/etc", `C:\\Users\\victim`, `server:stream`, "NUL/file",
	}
	for _, unsafePath := range cases {
		t.Run(unsafePath, func(t *testing.T) {
			_, err := service.AttachWorkspace(ctx, AttachWorkspaceRequest{
				Request: identity("unsafe-path"), VmID: "desktop", MountID: "source",
				RelativePath: unsafePath, ReadOnly: true,
			})
			assertCode(t, err, CodeInvalidArgument)
		})
	}

	_, err = service.AttachWorkspace(ctx, AttachWorkspaceRequest{
		Request: identity("writable"), VmID: "desktop", MountID: "source",
		RelativePath: "projects/demo", ReadOnly: false,
	})
	assertCode(t, err, CodePermissionDenied)
}

func TestStubHonorsSocketAllowlist(t *testing.T) {
	root := t.TempDir()
	service, err := NewStubService(Config{
		CurrentUserSID: testSID,
		ImageRoot:      filepath.Join(root, "images"), WorkspaceRoot: filepath.Join(root, "workspaces"),
		AllowedSocketPurposes: []SocketPurpose{SocketPurposeControl},
	})
	if err != nil {
		t.Fatalf("NewStubService: %v", err)
	}
	ensureTestImage(t, service)
	_, err = service.CreateVm(context.Background(), CreateVmRequest{
		Request: identity("create"), VmID: "desktop", ImageID: "guest-v1", VCPUCount: 2, MemoryMiB: 2048,
	})
	if err != nil {
		t.Fatalf("CreateVm: %v", err)
	}
	_, err = service.StartVm(context.Background(), StartVmRequest{Request: identity("start"), VmID: "desktop"})
	if err != nil {
		t.Fatalf("StartVm: %v", err)
	}
	_, err = service.OpenSocket(context.Background(), OpenSocketRequest{
		Request: identity("socket"), VmID: "desktop", Purpose: SocketPurposeComputerUseV1,
	})
	assertCode(t, err, CodePermissionDenied)
}

func TestStubRejectsOverlappingServiceRoots(t *testing.T) {
	root := t.TempDir()
	_, err := NewStubService(Config{
		CurrentUserSID: testSID,
		ImageRoot:      filepath.Join(root, "data"), WorkspaceRoot: filepath.Join(root, "data", "workspaces"),
	})
	assertCode(t, err, CodeInvalidArgument)
}

func newTestService(t *testing.T) *StubService {
	t.Helper()
	root := t.TempDir()
	service, err := NewPlatformService(Config{
		CurrentUserSID: testSID,
		ImageRoot:      filepath.Join(root, "images"), WorkspaceRoot: filepath.Join(root, "workspaces"),
	})
	if err != nil {
		t.Fatalf("NewPlatformService: %v", err)
	}
	stub, ok := service.(*StubService)
	if !ok {
		t.Fatalf("NewPlatformService returned %T, want *StubService", service)
	}
	return stub
}

func ensureTestImage(t *testing.T, service Service) {
	t.Helper()
	_, err := service.EnsureImage(context.Background(), EnsureImageRequest{
		Request: identity("ensure"), ImageID: "guest-v1", RelativePath: "images/guest.vhdx",
		SHA256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", SizeBytes: 4096,
	})
	if err != nil {
		t.Fatalf("EnsureImage: %v", err)
	}
}

func identity(requestID string) RequestIdentity {
	return RequestIdentity{RequestID: requestID, UserSID: testSID}
}

func assertCode(t *testing.T, err error, code ErrorCode) {
	t.Helper()
	if err == nil {
		t.Fatalf("expected %q error, got nil", code)
	}
	var serviceErr *Error
	if !errors.As(err, &serviceErr) {
		t.Fatalf("expected *Error, got %T: %v", err, err)
	}
	if serviceErr.Code != code {
		t.Fatalf("error code = %q, want %q: %v", serviceErr.Code, code, err)
	}
}
