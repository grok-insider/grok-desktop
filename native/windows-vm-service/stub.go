//go:build !windows

package vmservice

import (
	"context"
	"fmt"
	"runtime"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// StubService is a stateful, in-memory contract implementation. It is intended
// for non-Windows development and contract tests; Simulated is always true.
type StubService struct {
	mu        sync.Mutex
	config    normalizedConfig
	images    map[string]Image
	vms       map[string]*Vm
	socketSeq uint64
	closed    atomic.Bool
	now       func() time.Time
}

var _ Service = (*StubService)(nil)

func NewStubService(config Config) (*StubService, error) {
	normalized, err := normalizeConfig(config)
	if err != nil {
		return nil, err
	}
	return &StubService{
		config: normalized,
		images: make(map[string]Image),
		vms:    make(map[string]*Vm),
		now:    func() time.Time { return time.Now().UTC() },
	}, nil
}

func (s *StubService) GetCapabilities(ctx context.Context, request GetCapabilitiesRequest) (Capabilities, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return Capabilities{}, err
	}

	purposes := make([]SocketPurpose, 0, len(s.config.allowedSocketPurposes))
	for purpose := range s.config.allowedSocketPurposes {
		purposes = append(purposes, purpose)
	}
	sort.Slice(purposes, func(i, j int) bool { return purposes[i] < purposes[j] })

	return Capabilities{
		ContractVersion: ContractVersion,
		Backend:         "stub-" + runtime.GOOS,
		Simulated:       true,
		Available:       true,
		Operations: []Operation{
			OperationGetCapabilities,
			OperationEnsureImage,
			OperationCreateVM,
			OperationStartVM,
			OperationStopVM,
			OperationDeleteVM,
			OperationAttachWorkspace,
			OperationOpenSocket,
		},
		WorkspaceMode:  "read-only",
		SocketPurposes: purposes,
	}, nil
}

func (s *StubService) EnsureImage(ctx context.Context, request EnsureImageRequest) (Image, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return Image{}, err
	}
	if err := validateID("imageId", request.ImageID); err != nil {
		return Image{}, err
	}
	relativePath, err := resolveRelativePath(s.config.imageRoot, request.RelativePath)
	if err != nil {
		return Image{}, err
	}
	digest, err := validateSHA256(request.SHA256)
	if err != nil {
		return Image{}, err
	}
	if request.SizeBytes <= 0 {
		return Image{}, serviceError(CodeInvalidArgument, "sizeBytes must be positive")
	}

	image := Image{
		ID: request.ImageID, RelativePath: relativePath,
		SHA256: digest, SizeBytes: request.SizeBytes,
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if existing, ok := s.images[image.ID]; ok {
		if existing == image {
			return existing, nil
		}
		return Image{}, serviceError(CodeConflict, "image %q already exists with different immutable metadata", image.ID)
	}
	s.images[image.ID] = image
	return image, nil
}

func (s *StubService) CreateVm(ctx context.Context, request CreateVmRequest) (Vm, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return Vm{}, err
	}
	if err := validateID("vmId", request.VmID); err != nil {
		return Vm{}, err
	}
	if err := validateID("imageId", request.ImageID); err != nil {
		return Vm{}, err
	}
	if request.VCPUCount < 1 || request.VCPUCount > 32 {
		return Vm{}, serviceError(CodeInvalidArgument, "vcpuCount must be between 1 and 32")
	}
	if request.MemoryMiB < 512 || request.MemoryMiB > 65536 {
		return Vm{}, serviceError(CodeInvalidArgument, "memoryMiB must be between 512 and 65536")
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.images[request.ImageID]; !ok {
		return Vm{}, serviceError(CodeNotFound, "image %q has not been ensured", request.ImageID)
	}
	if _, ok := s.vms[request.VmID]; ok {
		return Vm{}, serviceError(CodeConflict, "VM %q already exists", request.VmID)
	}
	vm := &Vm{
		ID: request.VmID, ImageID: request.ImageID,
		VCPUCount: request.VCPUCount, MemoryMiB: request.MemoryMiB,
		State: VmStateCreated, Workspaces: []WorkspaceAttachment{}, UpdatedAt: s.now(),
	}
	s.vms[vm.ID] = vm
	return copyVm(vm), nil
}

func (s *StubService) StartVm(ctx context.Context, request StartVmRequest) (Vm, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return Vm{}, err
	}
	if err := validateID("vmId", request.VmID); err != nil {
		return Vm{}, err
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.vms[request.VmID]
	if !ok {
		return Vm{}, serviceError(CodeNotFound, "VM %q does not exist", request.VmID)
	}
	if vm.State != VmStateCreated && vm.State != VmStateStopped {
		return Vm{}, serviceError(CodeConflict, "VM %q cannot start from state %q", vm.ID, vm.State)
	}
	vm.State = VmStateRunning
	vm.UpdatedAt = s.now()
	return copyVm(vm), nil
}

func (s *StubService) StopVm(ctx context.Context, request StopVmRequest) (Vm, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return Vm{}, err
	}
	if err := validateID("vmId", request.VmID); err != nil {
		return Vm{}, err
	}
	if request.Mode == "" {
		request.Mode = StopModeGraceful
	}
	if request.Mode != StopModeGraceful && request.Mode != StopModeForce {
		return Vm{}, serviceError(CodeInvalidArgument, "mode must be %q or %q", StopModeGraceful, StopModeForce)
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.vms[request.VmID]
	if !ok {
		return Vm{}, serviceError(CodeNotFound, "VM %q does not exist", request.VmID)
	}
	if vm.State != VmStateRunning {
		return Vm{}, serviceError(CodeConflict, "VM %q cannot stop from state %q", vm.ID, vm.State)
	}
	vm.State = VmStateStopped
	vm.UpdatedAt = s.now()
	return copyVm(vm), nil
}

func (s *StubService) DeleteVm(ctx context.Context, request DeleteVmRequest) error {
	if err := s.begin(ctx, request.Request); err != nil {
		return err
	}
	if err := validateID("vmId", request.VmID); err != nil {
		return err
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.vms[request.VmID]
	if !ok {
		return serviceError(CodeNotFound, "VM %q does not exist", request.VmID)
	}
	if vm.State == VmStateRunning {
		return serviceError(CodeConflict, "VM %q must be stopped before deletion", vm.ID)
	}
	delete(s.vms, request.VmID)
	return nil
}

func (s *StubService) AttachWorkspace(ctx context.Context, request AttachWorkspaceRequest) (WorkspaceAttachment, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return WorkspaceAttachment{}, err
	}
	if err := validateID("vmId", request.VmID); err != nil {
		return WorkspaceAttachment{}, err
	}
	if err := validateID("mountId", request.MountID); err != nil {
		return WorkspaceAttachment{}, err
	}
	if !request.ReadOnly {
		return WorkspaceAttachment{}, serviceError(CodePermissionDenied, "workspace attachments must be read-only")
	}
	relativePath, err := resolveRelativePath(s.config.workspaceRoot, request.RelativePath)
	if err != nil {
		return WorkspaceAttachment{}, err
	}
	attachment := WorkspaceAttachment{MountID: request.MountID, RelativePath: relativePath, ReadOnly: true}

	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.vms[request.VmID]
	if !ok {
		return WorkspaceAttachment{}, serviceError(CodeNotFound, "VM %q does not exist", request.VmID)
	}
	if vm.State == VmStateRunning {
		return WorkspaceAttachment{}, serviceError(CodeConflict, "workspace attachments can only change while VM %q is stopped", vm.ID)
	}
	for _, existing := range vm.Workspaces {
		if existing.MountID != attachment.MountID {
			continue
		}
		if existing == attachment {
			return existing, nil
		}
		return WorkspaceAttachment{}, serviceError(CodeConflict, "mount %q already points to another workspace", attachment.MountID)
	}
	vm.Workspaces = append(vm.Workspaces, attachment)
	vm.UpdatedAt = s.now()
	return attachment, nil
}

func (s *StubService) OpenSocket(ctx context.Context, request OpenSocketRequest) (Socket, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return Socket{}, err
	}
	if err := validateID("vmId", request.VmID); err != nil {
		return Socket{}, err
	}
	if err := validateSocketPurpose(request.Purpose); err != nil {
		return Socket{}, err
	}
	if _, allowed := s.config.allowedSocketPurposes[request.Purpose]; !allowed {
		return Socket{}, serviceError(CodePermissionDenied, "socket purpose %q is disabled", request.Purpose)
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.vms[request.VmID]
	if !ok {
		return Socket{}, serviceError(CodeNotFound, "VM %q does not exist", request.VmID)
	}
	if vm.State != VmStateRunning {
		return Socket{}, serviceError(CodeConflict, "VM %q must be running before opening a socket", vm.ID)
	}
	s.socketSeq++
	id := socketID(s.socketSeq)
	return Socket{
		ID: id, VmID: vm.ID, Purpose: request.Purpose,
		Endpoint: fmt.Sprintf("stub://%s/%s/%s", vm.ID, request.Purpose, id),
	}, nil
}

func (s *StubService) GuestControl(ctx context.Context, request GuestControlRequest) (GuestControlResult, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return GuestControlResult{}, err
	}
	if err := validateID("vmId", request.VmID); err != nil {
		return GuestControlResult{}, err
	}
	if err := validateGuestControlMethod(request.Method); err != nil {
		return GuestControlResult{}, err
	}
	return GuestControlResult{}, serviceError(CodeUnavailable, "authenticated guest control is unavailable in the simulator")
}

func (s *StubService) begin(ctx context.Context, request RequestIdentity) error {
	if err := ctx.Err(); err != nil {
		return contextErrorMessage(err)
	}
	if s.closed.Load() {
		return serviceError(CodeUnavailable, "VM service tenant is closed")
	}
	return validateRequest(request, s.config.currentUserSID)
}

func (s *StubService) Close(context.Context) error {
	s.closed.Store(true)
	return nil
}

func (s *StubService) String() string {
	return strings.Join([]string{"vmservice", "stub", runtime.GOOS}, "/")
}
