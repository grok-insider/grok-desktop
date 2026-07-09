package vmservice

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/grok-insider/grok-desktop/native/windows-vm-service/internal/hcsapi"
)

const (
	stateVersion     = 1
	maxStateBytes    = 4 << 20
	maxImages        = 64
	maxVMs           = 64
	maxWorkspaces    = 32
	maxImageSizeByte = int64(128 << 30)
)

var guidPattern = regexp.MustCompile(`(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`)

type pathKind uint8

const (
	pathFile pathKind = iota
	pathDirectory
)

type fileIdentity struct {
	Volume   uint64 `json:"volume"`
	FileLow  uint64 `json:"fileLow"`
	FileHigh uint64 `json:"fileHigh"`
}

type validatedPath interface {
	Close() error
	File() *os.File
	Identity() fileIdentity
	Path() string
	Revalidate() error
}

type pathValidator interface {
	Open(string, string, pathKind) (validatedPath, error)
}

type storedImage struct {
	Image
	Identity fileIdentity `json:"identity"`
}

type storedWorkspace struct {
	WorkspaceAttachment
	Identity fileIdentity `json:"identity"`
}

type storedVM struct {
	ID               string            `json:"id"`
	HCSID            string            `json:"hcsId"`
	ImageID          string            `json:"imageId"`
	VCPUCount        uint16            `json:"vcpuCount"`
	MemoryMiB        uint32            `json:"memoryMiB"`
	State            VmState           `json:"state"`
	Workspaces       []storedWorkspace `json:"workspaces"`
	UpdatedAt        time.Time         `json:"updatedAt"`
	DiskRelativePath string            `json:"diskRelativePath"`
	DiskIdentity     fileIdentity      `json:"diskIdentity"`
	RuntimeID        string            `json:"runtimeId,omitempty"`
	PendingOperation string            `json:"pendingOperation,omitempty"`
	Deleting         bool              `json:"deleting,omitempty"`
}

type serviceState struct {
	Version int                    `json:"version"`
	Images  map[string]storedImage `json:"images"`
	VMs     map[string]*storedVM   `json:"vms"`
}

type hcsService struct {
	mu       sync.Mutex
	config   normalizedConfig
	client   hcsapi.Client
	paths    pathValidator
	owner    string
	state    serviceState
	channels *guestChannelPool
	closed   atomic.Bool
	now      func() time.Time
}

var _ Service = (*hcsService)(nil)

func newHCSService(ctx context.Context, config Config, client hcsapi.Client, paths pathValidator) (*hcsService, error) {
	return newHCSServiceWithGuestDialer(ctx, config, client, paths, newPlatformGuestSocketDialer())
}

func newHCSServiceWithGuestDialer(
	ctx context.Context,
	config Config,
	client hcsapi.Client,
	paths pathValidator,
	dialer guestSocketDialer,
) (*hcsService, error) {
	normalized, err := normalizeConfig(config)
	if err != nil {
		return nil, err
	}
	if err := validateNativeConfig(normalized); err != nil {
		return nil, err
	}
	if normalized.guestImagePolicy == nil {
		return nil, serviceError(CodeUnavailable, "verified guest image policy is required")
	}
	if client == nil || paths == nil || dialer == nil {
		return nil, serviceError(CodeUnavailable, "HCS dependencies are not configured")
	}
	if err := os.MkdirAll(normalized.installedRoot, 0o700); err != nil {
		return nil, serviceError(CodeUnavailable, "create image store: %v", err)
	}
	if err := os.MkdirAll(normalized.vmRoot, 0o700); err != nil {
		return nil, serviceError(CodeUnavailable, "create VM store: %v", err)
	}
	for _, root := range []struct {
		base     string
		relative string
		name     string
	}{
		{normalized.imageRoot, ".", "image root"},
		{normalized.workspaceRoot, ".", "workspace root"},
		{normalized.imageRoot, ".vm-service", "metadata root"},
		{normalized.imageRoot, ".vm-service/installed", "installed image root"},
		{normalized.imageRoot, ".vm-service/vms", "VM disk root"},
	} {
		handle, openErr := paths.Open(root.base, root.relative, pathDirectory)
		if openErr != nil {
			return nil, pathServiceError("validate "+root.name, openErr)
		}
		_ = handle.Close()
	}
	if err := client.Probe(ctx); err != nil {
		return nil, hcsServiceError("probe HCS and VirtualMachinePlatform", err)
	}

	service := &hcsService{
		config:   normalized,
		client:   client,
		paths:    paths,
		owner:    deriveOwner(normalized.currentUserSID),
		channels: newGuestChannelPool(dialer, normalized.guestControlMaxBytes),
		now:      func() time.Time { return time.Now().UTC() },
	}
	if err := service.loadState(); err != nil {
		return nil, err
	}
	if err := service.reconcileLocked(ctx); err != nil {
		service.channels.close()
		return nil, err
	}
	if err := service.recoverGuestChannels(ctx); err != nil {
		service.channels.close()
		return nil, err
	}
	return service, nil
}

func (s *hcsService) recoverGuestChannels(ctx context.Context) error {
	if _, required := s.config.allowedSocketPurposes[SocketPurposeControl]; !required {
		return nil
	}
	ids := make([]string, 0, len(s.state.VMs))
	for id, vm := range s.state.VMs {
		if vm.State == VmStateRunning {
			ids = append(ids, id)
		}
	}
	sort.Strings(ids)
	changed := false
	for _, id := range ids {
		if err := ctx.Err(); err != nil {
			return contextErrorMessage(err)
		}
		vm := s.state.VMs[id]
		key := guestChannelKey{vmID: vm.ID, runtimeID: vm.RuntimeID, purpose: SocketPurposeControl}
		if err := s.channels.ensure(ctx, key); err == nil {
			continue
		}
		s.channels.closeKey(key)
		if err := s.client.Terminate(ctx, vm.HCSID); err != nil && !hcsNotFound(err) {
			return hcsServiceError("terminate VM after guest channel recovery failure", err)
		}
		vm.State, vm.RuntimeID, vm.PendingOperation, vm.UpdatedAt = VmStateStopped, "", "", s.now()
		changed = true
	}
	if changed {
		return s.persistLocked()
	}
	return nil
}

func (s *hcsService) GetCapabilities(ctx context.Context, request GetCapabilitiesRequest) (Capabilities, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return Capabilities{}, err
	}
	if err := s.client.Probe(ctx); err != nil {
		return Capabilities{}, hcsServiceError("probe HCS readiness", err)
	}
	return Capabilities{
		ContractVersion: ContractVersion,
		Backend:         "hcs-virtual-machine-platform",
		Simulated:       false,
		Available:       true,
		HCSSchema:       "2.1",
		Operations: []Operation{
			OperationGetCapabilities, OperationEnsureImage, OperationCreateVM, OperationStartVM,
			OperationStopVM, OperationDeleteVM, OperationAttachWorkspace,
		},
		WorkspaceMode:  "read-only-plan9",
		SocketPurposes: []SocketPurpose{},
	}, nil
}

func (s *hcsService) EnsureImage(ctx context.Context, request EnsureImageRequest) (Image, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return Image{}, err
	}
	if err := validateID("imageId", request.ImageID); err != nil {
		return Image{}, err
	}
	trusted, allowed := s.config.guestImagePolicy.image(request.ImageID)
	if !allowed {
		return Image{}, serviceError(CodePermissionDenied, "image is not present in the signed guest catalog")
	}
	relativePath, err := resolveRelativePath(s.config.imageRoot, request.RelativePath)
	if err != nil {
		return Image{}, err
	}
	expectedSource := filepath.ToSlash(filepath.Join("staging", trusted.StagingName))
	if filepath.ToSlash(relativePath) != expectedSource {
		return Image{}, serviceError(CodePermissionDenied, "image source does not match the signed staging name")
	}
	if request.SHA256 != "" {
		digest, digestErr := validateSHA256(request.SHA256)
		if digestErr != nil {
			return Image{}, digestErr
		}
		if digest != trusted.SHA256 {
			return Image{}, serviceError(CodeInvalidArgument, "caller image digest does not match signed release metadata")
		}
	}
	if request.SizeBytes != 0 && request.SizeBytes != trusted.SizeBytes {
		return Image{}, serviceError(CodeInvalidArgument, "caller image size does not match signed release metadata")
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if existing, ok := s.state.Images[request.ImageID]; ok {
		if err := s.trustedStoredImage(existing); err != nil {
			return Image{}, err
		}
		if err := s.verifyStoredImage(ctx, existing); err != nil {
			return Image{}, err
		}
		return existing.Image, nil
	}
	if len(s.state.Images) >= maxImages {
		return Image{}, serviceError(CodeConflict, "installed image limit of %d reached", maxImages)
	}

	source, err := s.paths.Open(s.config.imageRoot, relativePath, pathFile)
	if err != nil {
		return Image{}, pathServiceError("open staged image", err)
	}
	defer source.Close()
	destinationRelative := filepath.ToSlash(filepath.Join(".vm-service", "installed", request.ImageID, "disk.vhdx"))
	destination := filepath.Join(s.config.imageRoot, filepath.FromSlash(destinationRelative))
	if err := copyVerified(ctx, source.File(), destination, trusted.SHA256, trusted.SizeBytes, CodeInvalidArgument); err != nil {
		return Image{}, err
	}
	if err := source.Revalidate(); err != nil {
		_ = os.Remove(destination)
		return Image{}, pathServiceError("revalidate staged image", err)
	}
	installed, err := s.paths.Open(s.config.imageRoot, destinationRelative, pathFile)
	if err != nil {
		_ = os.Remove(destination)
		return Image{}, pathServiceError("open installed image", err)
	}
	identity := installed.Identity()
	_ = installed.Close()
	image := storedImage{
		Image:    Image{ID: request.ImageID, RelativePath: destinationRelative, SHA256: trusted.SHA256, SizeBytes: trusted.SizeBytes},
		Identity: identity,
	}
	s.state.Images[request.ImageID] = image
	if err := s.persistLocked(); err != nil {
		delete(s.state.Images, request.ImageID)
		_ = os.RemoveAll(filepath.Dir(destination))
		return Image{}, err
	}
	return image.Image, nil
}

func (s *hcsService) CreateVm(ctx context.Context, request CreateVmRequest) (Vm, error) {
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
	image, ok := s.state.Images[request.ImageID]
	if !ok {
		return Vm{}, serviceError(CodeNotFound, "image %q has not been ensured", request.ImageID)
	}
	if err := s.trustedStoredImage(image); err != nil {
		return Vm{}, err
	}
	if _, exists := s.state.VMs[request.VmID]; exists {
		return Vm{}, serviceError(CodeConflict, "VM %q already exists", request.VmID)
	}
	if len(s.state.VMs) >= maxVMs {
		return Vm{}, serviceError(CodeConflict, "VM limit of %d reached", maxVMs)
	}
	source, err := s.paths.Open(s.config.imageRoot, image.RelativePath, pathFile)
	if err != nil {
		return Vm{}, pathServiceError("open installed image", err)
	}
	defer source.Close()
	if source.Identity() != image.Identity {
		return Vm{}, serviceError(CodePermissionDenied, "installed image identity changed")
	}
	diskRelative := filepath.ToSlash(filepath.Join(".vm-service", "vms", request.VmID, "disk.vhdx"))
	diskPath := filepath.Join(s.config.imageRoot, filepath.FromSlash(diskRelative))
	if err := copyVerified(ctx, source.File(), diskPath, image.SHA256, image.SizeBytes, CodePermissionDenied); err != nil {
		return Vm{}, err
	}
	if err := source.Revalidate(); err != nil {
		_ = os.RemoveAll(filepath.Dir(diskPath))
		return Vm{}, pathServiceError("revalidate installed image", err)
	}
	disk, err := s.paths.Open(s.config.imageRoot, diskRelative, pathFile)
	if err != nil {
		_ = os.RemoveAll(filepath.Dir(diskPath))
		return Vm{}, pathServiceError("open VM disk", err)
	}
	identity := disk.Identity()
	_ = disk.Close()
	now := s.now()
	vm := &storedVM{
		ID: request.VmID, HCSID: deriveHCSID(s.config.currentUserSID, request.VmID), ImageID: request.ImageID,
		VCPUCount: request.VCPUCount, MemoryMiB: request.MemoryMiB, State: VmStateCreated,
		Workspaces: []storedWorkspace{}, UpdatedAt: now, DiskRelativePath: diskRelative, DiskIdentity: identity,
	}
	s.state.VMs[request.VmID] = vm
	if err := s.persistLocked(); err != nil {
		delete(s.state.VMs, request.VmID)
		_ = os.RemoveAll(filepath.Dir(diskPath))
		return Vm{}, err
	}
	return vm.public(), nil
}

func (s *hcsService) StartVm(ctx context.Context, request StartVmRequest) (Vm, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return Vm{}, err
	}
	if err := validateID("vmId", request.VmID); err != nil {
		return Vm{}, err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.state.VMs[request.VmID]
	if !ok || vm.Deleting {
		return Vm{}, serviceError(CodeNotFound, "VM %q does not exist", request.VmID)
	}
	if vm.State != VmStateCreated && vm.State != VmStateStopped {
		return Vm{}, serviceError(CodeConflict, "VM %q cannot start from state %q", vm.ID, vm.State)
	}
	image, exists := s.state.Images[vm.ImageID]
	if !exists {
		return Vm{}, serviceError(CodePermissionDenied, "VM source image metadata is unavailable")
	}
	if err := s.trustedStoredImage(image); err != nil {
		return Vm{}, err
	}

	disk, err := s.paths.Open(s.config.imageRoot, vm.DiskRelativePath, pathFile)
	if err != nil {
		return Vm{}, pathServiceError("open VM disk", err)
	}
	defer disk.Close()
	if disk.Identity() != vm.DiskIdentity {
		return Vm{}, serviceError(CodePermissionDenied, "VM disk identity changed")
	}

	workspaceHandles := make([]validatedPath, 0, len(vm.Workspaces))
	resolved := make([]resolvedWorkspace, 0, len(vm.Workspaces))
	defer func() {
		for _, handle := range workspaceHandles {
			_ = handle.Close()
		}
	}()
	for _, workspace := range vm.Workspaces {
		handle, openErr := s.paths.Open(s.config.workspaceRoot, workspace.RelativePath, pathDirectory)
		if openErr != nil {
			return Vm{}, pathServiceError("open workspace", openErr)
		}
		workspaceHandles = append(workspaceHandles, handle)
		if handle.Identity() != workspace.Identity {
			return Vm{}, serviceError(CodePermissionDenied, "workspace %q identity changed", workspace.MountID)
		}
		resolved = append(resolved, resolvedWorkspace{MountID: workspace.MountID, Path: handle.Path()})
	}
	document, err := buildHCSDocument(vm, disk.Path(), s.owner, resolved, s.config.allowedSocketPurposes)
	if err != nil {
		return Vm{}, err
	}
	vm.PendingOperation = "starting"
	if err := s.persistLocked(); err != nil {
		vm.PendingOperation = ""
		return Vm{}, err
	}
	if err := s.client.GrantVMAccess(ctx, vm.HCSID, disk.Path()); err != nil {
		s.clearPendingLocked(vm)
		return Vm{}, hcsServiceError("grant VM disk access", err)
	}
	if err := s.client.Create(ctx, vm.HCSID, document); err != nil {
		_ = s.client.Terminate(context.Background(), vm.HCSID)
		_ = s.client.RevokeVMAccess(context.Background(), vm.HCSID, disk.Path())
		s.clearPendingLocked(vm)
		return Vm{}, hcsServiceError("create HCS virtual machine", err)
	}
	for _, handle := range append([]validatedPath{disk}, workspaceHandles...) {
		if err := handle.Revalidate(); err != nil {
			_ = s.client.Terminate(context.Background(), vm.HCSID)
			_ = s.client.RevokeVMAccess(context.Background(), vm.HCSID, disk.Path())
			s.clearPendingLocked(vm)
			return Vm{}, pathServiceError("revalidate HCS resource", err)
		}
	}
	if err := s.client.Start(ctx, vm.HCSID); err != nil {
		_ = s.client.Terminate(context.Background(), vm.HCSID)
		s.clearPendingLocked(vm)
		return Vm{}, hcsServiceError("start HCS virtual machine", err)
	}
	runtimeID, err := s.runtimeID(ctx, vm.HCSID)
	if err != nil {
		_ = s.client.Terminate(context.Background(), vm.HCSID)
		_ = s.client.RevokeVMAccess(context.Background(), vm.HCSID, disk.Path())
		s.clearPendingLocked(vm)
		return Vm{}, err
	}
	if _, required := s.config.allowedSocketPurposes[SocketPurposeControl]; required {
		key := guestChannelKey{vmID: vm.ID, runtimeID: runtimeID, purpose: SocketPurposeControl}
		if err := s.channels.ensure(ctx, key); err != nil {
			s.channels.closeKey(key)
			_ = s.client.Terminate(context.Background(), vm.HCSID)
			_ = s.client.RevokeVMAccess(context.Background(), vm.HCSID, disk.Path())
			s.clearPendingLocked(vm)
			return Vm{}, guestChannelServiceError(ctx, err)
		}
	}
	previousState, previousRuntime, previousTime := vm.State, vm.RuntimeID, vm.UpdatedAt
	vm.State, vm.RuntimeID, vm.PendingOperation, vm.UpdatedAt = VmStateRunning, runtimeID, "", s.now()
	if err := s.persistLocked(); err != nil {
		vm.State, vm.RuntimeID, vm.PendingOperation, vm.UpdatedAt = previousState, previousRuntime, "", previousTime
		s.channels.closeVM(vm.ID)
		_ = s.client.Terminate(context.Background(), vm.HCSID)
		_ = s.client.RevokeVMAccess(context.Background(), vm.HCSID, disk.Path())
		_ = s.persistLocked()
		return Vm{}, err
	}
	return vm.public(), nil
}

func (s *hcsService) StopVm(ctx context.Context, request StopVmRequest) (Vm, error) {
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
	vm, ok := s.state.VMs[request.VmID]
	if !ok || vm.Deleting {
		return Vm{}, serviceError(CodeNotFound, "VM %q does not exist", request.VmID)
	}
	if vm.State != VmStateRunning {
		return Vm{}, serviceError(CodeConflict, "VM %q cannot stop from state %q", vm.ID, vm.State)
	}
	s.channels.closeVM(vm.ID)
	vm.PendingOperation = "stopping"
	if err := s.persistLocked(); err != nil {
		vm.PendingOperation = ""
		return Vm{}, err
	}
	var err error
	if request.Mode == StopModeForce {
		err = s.client.Terminate(ctx, vm.HCSID)
	} else {
		err = s.client.Shutdown(ctx, vm.HCSID)
	}
	if err != nil {
		s.clearPendingLocked(vm)
		return Vm{}, hcsServiceError("stop HCS virtual machine", err)
	}
	vm.State, vm.RuntimeID, vm.PendingOperation, vm.UpdatedAt = VmStateStopped, "", "", s.now()
	if err := s.persistLocked(); err != nil {
		return Vm{}, err
	}
	return vm.public(), nil
}

func (s *hcsService) DeleteVm(ctx context.Context, request DeleteVmRequest) error {
	if err := s.begin(ctx, request.Request); err != nil {
		return err
	}
	if err := validateID("vmId", request.VmID); err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.state.VMs[request.VmID]
	if !ok {
		return serviceError(CodeNotFound, "VM %q does not exist", request.VmID)
	}
	if vm.State == VmStateRunning {
		return serviceError(CodeConflict, "VM %q must be stopped before deletion", vm.ID)
	}
	s.channels.closeVM(vm.ID)
	vm.Deleting = true
	if err := s.persistLocked(); err != nil {
		vm.Deleting = false
		return err
	}
	diskPath := filepath.Join(s.config.imageRoot, filepath.FromSlash(vm.DiskRelativePath))
	revokePath := diskPath
	disk, openErr := s.paths.Open(s.config.imageRoot, vm.DiskRelativePath, pathFile)
	if openErr == nil {
		if disk.Identity() != vm.DiskIdentity {
			_ = disk.Close()
			return serviceError(CodePermissionDenied, "VM disk identity changed")
		}
		revokePath = disk.Path()
		_ = disk.Close()
	} else if !errors.Is(openErr, os.ErrNotExist) {
		return pathServiceError("open VM disk for deletion", openErr)
	}
	if err := s.client.RevokeVMAccess(ctx, vm.HCSID, revokePath); err != nil && !hcsNotFound(err) {
		return hcsServiceError("revoke VM disk access", err)
	}
	if err := os.RemoveAll(filepath.Dir(diskPath)); err != nil {
		return serviceError(CodeUnavailable, "delete VM disk: %v", err)
	}
	delete(s.state.VMs, request.VmID)
	return s.persistLocked()
}

func (s *hcsService) AttachWorkspace(ctx context.Context, request AttachWorkspaceRequest) (WorkspaceAttachment, error) {
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
	relative, err := resolveRelativePath(s.config.workspaceRoot, request.RelativePath)
	if err != nil {
		return WorkspaceAttachment{}, err
	}
	handle, err := s.paths.Open(s.config.workspaceRoot, relative, pathDirectory)
	if err != nil {
		return WorkspaceAttachment{}, pathServiceError("open workspace", err)
	}
	defer handle.Close()
	attachment := WorkspaceAttachment{MountID: request.MountID, RelativePath: relative, ReadOnly: true}
	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.state.VMs[request.VmID]
	if !ok || vm.Deleting {
		return WorkspaceAttachment{}, serviceError(CodeNotFound, "VM %q does not exist", request.VmID)
	}
	if vm.State == VmStateRunning {
		return WorkspaceAttachment{}, serviceError(CodeConflict, "workspace attachments can only change while VM %q is stopped", vm.ID)
	}
	for _, existing := range vm.Workspaces {
		if existing.MountID != attachment.MountID {
			continue
		}
		if existing.WorkspaceAttachment == attachment && existing.Identity == handle.Identity() {
			return attachment, nil
		}
		return WorkspaceAttachment{}, serviceError(CodeConflict, "mount %q already points to another workspace", attachment.MountID)
	}
	if len(vm.Workspaces) >= maxWorkspaces {
		return WorkspaceAttachment{}, serviceError(CodeConflict, "workspace attachment limit of %d reached", maxWorkspaces)
	}
	vm.Workspaces = append(vm.Workspaces, storedWorkspace{WorkspaceAttachment: attachment, Identity: handle.Identity()})
	vm.UpdatedAt = s.now()
	if err := s.persistLocked(); err != nil {
		vm.Workspaces = vm.Workspaces[:len(vm.Workspaces)-1]
		return WorkspaceAttachment{}, err
	}
	return attachment, nil
}

func (s *hcsService) OpenSocket(ctx context.Context, request OpenSocketRequest) (Socket, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return Socket{}, err
	}
	if err := validateID("vmId", request.VmID); err != nil {
		return Socket{}, err
	}
	if err := validateSocketPurpose(request.Purpose); err != nil {
		return Socket{}, err
	}
	return Socket{}, serviceError(CodeUnavailable, "raw guest sockets are disabled until the authenticated service proxy is available")
}

func (s *hcsService) begin(ctx context.Context, request RequestIdentity) error {
	if err := ctx.Err(); err != nil {
		return contextErrorMessage(err)
	}
	if s.closed.Load() {
		return serviceError(CodeUnavailable, "VM service tenant is closed")
	}
	return validateRequest(request, s.config.currentUserSID)
}

func (s *hcsService) Close(context.Context) error {
	if s.closed.CompareAndSwap(false, true) {
		s.channels.close()
	}
	return nil
}

func (s *hcsService) loadState() error {
	validated, err := s.paths.Open(s.config.imageRoot, ".vm-service/state.json", pathFile)
	if errors.Is(err, os.ErrNotExist) {
		s.state = serviceState{Version: stateVersion, Images: map[string]storedImage{}, VMs: map[string]*storedVM{}}
		return s.persistLocked()
	}
	if err != nil {
		return serviceError(CodeUnavailable, "open VM metadata: %v", err)
	}
	defer validated.Close()
	file := validated.File()
	information, err := file.Stat()
	if err != nil {
		return serviceError(CodeUnavailable, "inspect VM metadata: %v", err)
	}
	if information.Size() > maxStateBytes {
		return serviceError(CodeUnavailable, "VM metadata exceeds its size limit")
	}
	decoder := json.NewDecoder(io.LimitReader(file, maxStateBytes))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&s.state); err != nil {
		return serviceError(CodeUnavailable, "decode VM metadata: %v", err)
	}
	if err := ensureJSONEOF(decoder); err != nil {
		return serviceError(CodeUnavailable, "decode VM metadata: %v", err)
	}
	if s.state.Version != stateVersion || s.state.Images == nil || s.state.VMs == nil {
		return serviceError(CodeUnavailable, "VM metadata has an unsupported or incomplete schema")
	}
	return s.validateState()
}

func (s *hcsService) validateState() error {
	if len(s.state.Images) > maxImages || len(s.state.VMs) > maxVMs {
		return serviceError(CodeUnavailable, "VM metadata exceeds its resource limits")
	}
	for id, image := range s.state.Images {
		if validateID("stored imageId", id) != nil || image.ID != id || image.SizeBytes <= 0 {
			return serviceError(CodeUnavailable, "VM metadata contains an invalid image record")
		}
		if _, err := validateSHA256(image.SHA256); err != nil {
			return serviceError(CodeUnavailable, "VM metadata contains an invalid image digest")
		}
		expected := filepath.ToSlash(filepath.Join(".vm-service", "installed", id, "disk.vhdx"))
		if image.RelativePath != expected {
			return serviceError(CodeUnavailable, "VM metadata image path is outside its fixed root")
		}
		if err := s.trustedStoredImage(image); err != nil {
			return serviceError(CodeUnavailable, "VM metadata contains an image outside the signed guest catalog")
		}
	}
	for id, vm := range s.state.VMs {
		if vm == nil || validateID("stored vmId", id) != nil || vm.ID != id || vm.HCSID != deriveHCSID(s.config.currentUserSID, id) || vm.UpdatedAt.IsZero() {
			return serviceError(CodeUnavailable, "VM metadata contains an invalid VM identity")
		}
		if _, ok := s.state.Images[vm.ImageID]; !ok || vm.VCPUCount < 1 || vm.VCPUCount > 32 || vm.MemoryMiB < 512 || vm.MemoryMiB > 65536 {
			return serviceError(CodeUnavailable, "VM metadata contains an invalid VM resource record")
		}
		expectedDisk := filepath.ToSlash(filepath.Join(".vm-service", "vms", id, "disk.vhdx"))
		if vm.DiskRelativePath != expectedDisk {
			return serviceError(CodeUnavailable, "VM metadata disk path is outside its fixed root")
		}
		if vm.State != VmStateCreated && vm.State != VmStateRunning && vm.State != VmStateStopped {
			return serviceError(CodeUnavailable, "VM metadata contains an invalid lifecycle state")
		}
		if vm.RuntimeID != "" && !guidPattern.MatchString(vm.RuntimeID) {
			return serviceError(CodeUnavailable, "VM metadata contains an invalid runtime identity")
		}
		if vm.PendingOperation != "" && vm.PendingOperation != "starting" && vm.PendingOperation != "stopping" {
			return serviceError(CodeUnavailable, "VM metadata contains an invalid pending operation")
		}
		if len(vm.Workspaces) > maxWorkspaces {
			return serviceError(CodeUnavailable, "VM metadata exceeds its workspace limit")
		}
		mounts := make(map[string]struct{}, len(vm.Workspaces))
		for _, workspace := range vm.Workspaces {
			if validateID("stored mountId", workspace.MountID) != nil || !workspace.ReadOnly {
				return serviceError(CodeUnavailable, "VM metadata contains an invalid workspace attachment")
			}
			if _, err := resolveRelativePath(s.config.workspaceRoot, workspace.RelativePath); err != nil {
				return serviceError(CodeUnavailable, "VM metadata workspace path is outside its fixed root")
			}
			if _, exists := mounts[workspace.MountID]; exists {
				return serviceError(CodeUnavailable, "VM metadata contains duplicate workspace attachments")
			}
			mounts[workspace.MountID] = struct{}{}
		}
	}
	return nil
}

func (s *hcsService) persistLocked() error {
	if err := os.MkdirAll(s.config.stateRoot, 0o700); err != nil {
		return serviceError(CodeUnavailable, "create metadata directory: %v", err)
	}
	stateRoot, err := s.paths.Open(s.config.imageRoot, ".vm-service", pathDirectory)
	if err != nil {
		return pathServiceError("validate metadata transaction root", err)
	}
	defer stateRoot.Close()
	temporary, err := os.CreateTemp(s.config.stateRoot, ".state-*.json")
	if err != nil {
		return serviceError(CodeUnavailable, "create metadata transaction: %v", err)
	}
	temporaryPath := temporary.Name()
	committed := false
	defer func() {
		_ = temporary.Close()
		if !committed {
			_ = os.Remove(temporaryPath)
		}
	}()
	if err := temporary.Chmod(0o600); err != nil {
		return serviceError(CodeUnavailable, "secure metadata transaction: %v", err)
	}
	encoder := json.NewEncoder(temporary)
	if err := encoder.Encode(s.state); err != nil {
		return serviceError(CodeUnavailable, "encode VM metadata: %v", err)
	}
	if err := temporary.Sync(); err != nil {
		return serviceError(CodeUnavailable, "flush VM metadata: %v", err)
	}
	if err := temporary.Close(); err != nil {
		return serviceError(CodeUnavailable, "close VM metadata: %v", err)
	}
	if err := atomicReplace(temporaryPath, filepath.Join(s.config.stateRoot, "state.json")); err != nil {
		return serviceError(CodeUnavailable, "commit VM metadata: %v", err)
	}
	committed = true
	return nil
}

func (s *hcsService) reconcileLocked(ctx context.Context) error {
	systems, err := s.client.Enumerate(ctx, s.owner)
	if err != nil {
		return hcsServiceError("enumerate HCS virtual machines", err)
	}
	byID := make(map[string]hcsapi.System, len(systems))
	known := make(map[string]*storedVM, len(s.state.VMs))
	changed := false
	for id, vm := range s.state.VMs {
		if vm == nil || vm.ID != id || vm.HCSID == "" {
			return serviceError(CodeUnavailable, "VM metadata contains an invalid record")
		}
		known[vm.HCSID] = vm
	}
	for _, system := range systems {
		if system.Owner != s.owner || !strings.EqualFold(system.Type, "VirtualMachine") {
			continue
		}
		byID[system.ID] = system
		if _, ok := known[system.ID]; !ok {
			if err := s.client.Terminate(ctx, system.ID); err != nil && !hcsNotFound(err) {
				return hcsServiceError("terminate orphaned HCS virtual machine", err)
			}
		}
	}
	for id, vm := range s.state.VMs {
		if vm.Deleting {
			if system, ok := byID[vm.HCSID]; ok && !strings.EqualFold(system.State, "Stopped") {
				if err := s.client.Terminate(ctx, vm.HCSID); err != nil && !hcsNotFound(err) {
					return hcsServiceError("finish interrupted VM deletion", err)
				}
			}
			_ = os.RemoveAll(filepath.Join(s.config.vmRoot, id))
			delete(s.state.VMs, id)
			changed = true
			continue
		}
		system, exists := byID[vm.HCSID]
		if exists && strings.EqualFold(system.State, "Running") && guidPattern.MatchString(system.RuntimeID) {
			if vm.State != VmStateRunning || !strings.EqualFold(vm.RuntimeID, system.RuntimeID) || vm.PendingOperation != "" {
				vm.State, vm.RuntimeID, vm.PendingOperation, vm.UpdatedAt = VmStateRunning, strings.ToLower(system.RuntimeID), "", s.now()
				changed = true
			}
			continue
		}
		if exists {
			if err := s.client.Terminate(ctx, vm.HCSID); err != nil && !hcsNotFound(err) {
				return hcsServiceError("clean incomplete HCS virtual machine", err)
			}
		}
		if vm.State == VmStateRunning || vm.RuntimeID != "" || vm.PendingOperation != "" {
			if vm.State == VmStateRunning || vm.PendingOperation == "stopping" {
				vm.State = VmStateStopped
			}
			vm.RuntimeID, vm.PendingOperation, vm.UpdatedAt = "", "", s.now()
			changed = true
		}
	}
	entries, err := os.ReadDir(s.config.vmRoot)
	if err != nil {
		return serviceError(CodeUnavailable, "scan VM disk store: %v", err)
	}
	for _, entry := range entries {
		if _, ok := s.state.VMs[entry.Name()]; !ok {
			if err := os.RemoveAll(filepath.Join(s.config.vmRoot, entry.Name())); err != nil {
				return serviceError(CodeUnavailable, "remove orphaned VM disk: %v", err)
			}
		}
	}
	entries, err = os.ReadDir(s.config.installedRoot)
	if err != nil {
		return serviceError(CodeUnavailable, "scan installed image store: %v", err)
	}
	for _, entry := range entries {
		if _, ok := s.state.Images[entry.Name()]; !ok {
			if err := os.RemoveAll(filepath.Join(s.config.installedRoot, entry.Name())); err != nil {
				return serviceError(CodeUnavailable, "remove orphaned installed image: %v", err)
			}
		}
	}
	if changed {
		return s.persistLocked()
	}
	return nil
}

func (s *hcsService) runtimeID(ctx context.Context, hcsID string) (string, error) {
	for attempt := 0; attempt < 3; attempt++ {
		systems, err := s.client.Enumerate(ctx, s.owner)
		if err != nil {
			return "", hcsServiceError("read HCS runtime identity", err)
		}
		for _, system := range systems {
			if system.ID == hcsID && strings.EqualFold(system.State, "Running") && guidPattern.MatchString(system.RuntimeID) {
				return strings.ToLower(system.RuntimeID), nil
			}
		}
		if attempt < 2 {
			select {
			case <-ctx.Done():
				return "", contextErrorMessage(ctx.Err())
			case <-time.After(50 * time.Millisecond):
			}
		}
	}
	return "", serviceError(CodeUnavailable, "HCS did not publish a valid running runtime identity")
}

func (s *hcsService) verifyStoredImage(ctx context.Context, image storedImage) error {
	handle, err := s.paths.Open(s.config.imageRoot, image.RelativePath, pathFile)
	if err != nil {
		return pathServiceError("open installed image", err)
	}
	defer handle.Close()
	if handle.Identity() != image.Identity {
		return serviceError(CodePermissionDenied, "installed image identity changed")
	}
	if err := verifyReader(ctx, handle.File(), image.SHA256, image.SizeBytes); err != nil {
		return err
	}
	if err := handle.Revalidate(); err != nil {
		return pathServiceError("revalidate installed image", err)
	}
	return nil
}

func (s *hcsService) trustedStoredImage(image storedImage) error {
	trusted, allowed := s.config.guestImagePolicy.image(image.ID)
	if !allowed || image.SHA256 != trusted.SHA256 || image.SizeBytes != trusted.SizeBytes {
		return serviceError(CodePermissionDenied, "image metadata does not match the signed guest catalog")
	}
	return nil
}

func (s *hcsService) clearPendingLocked(vm *storedVM) {
	vm.PendingOperation = ""
	_ = s.persistLocked()
}

func (vm *storedVM) public() Vm {
	workspaces := make([]WorkspaceAttachment, 0, len(vm.Workspaces))
	for _, workspace := range vm.Workspaces {
		workspaces = append(workspaces, workspace.WorkspaceAttachment)
	}
	return Vm{
		ID: vm.ID, ImageID: vm.ImageID, VCPUCount: vm.VCPUCount, MemoryMiB: vm.MemoryMiB,
		State: vm.State, Workspaces: workspaces, UpdatedAt: vm.UpdatedAt,
	}
}

func copyVerified(ctx context.Context, source *os.File, destination, digest string, size int64, mismatchCode ErrorCode) error {
	if _, err := source.Seek(0, io.SeekStart); err != nil {
		return serviceError(CodeUnavailable, "seek image source: %v", err)
	}
	if err := os.MkdirAll(filepath.Dir(destination), 0o700); err != nil {
		return serviceError(CodeUnavailable, "create image destination: %v", err)
	}
	temporary, err := os.CreateTemp(filepath.Dir(destination), ".copy-*.tmp")
	if err != nil {
		return serviceError(CodeUnavailable, "create image transaction: %v", err)
	}
	temporaryPath := temporary.Name()
	committed := false
	defer func() {
		_ = temporary.Close()
		if !committed {
			_ = os.Remove(temporaryPath)
		}
	}()
	if err := temporary.Chmod(0o600); err != nil {
		return serviceError(CodeUnavailable, "secure image transaction: %v", err)
	}
	hash := sha256.New()
	written, err := copyContext(ctx, io.MultiWriter(temporary, hash), source)
	if err != nil {
		return err
	}
	actualDigest := hex.EncodeToString(hash.Sum(nil))
	if written != size || actualDigest != digest {
		return serviceError(mismatchCode, "image content does not match declared size and SHA-256")
	}
	if err := temporary.Sync(); err != nil {
		return serviceError(CodeUnavailable, "flush image transaction: %v", err)
	}
	if err := temporary.Close(); err != nil {
		return serviceError(CodeUnavailable, "close image transaction: %v", err)
	}
	if err := atomicReplace(temporaryPath, destination); err != nil {
		return serviceError(CodeUnavailable, "commit image transaction: %v", err)
	}
	committed = true
	return nil
}

func verifyReader(ctx context.Context, source *os.File, digest string, size int64) error {
	if _, err := source.Seek(0, io.SeekStart); err != nil {
		return serviceError(CodeUnavailable, "seek image: %v", err)
	}
	hash := sha256.New()
	read, err := copyContext(ctx, hash, source)
	if err != nil {
		return err
	}
	if read != size || hex.EncodeToString(hash.Sum(nil)) != digest {
		return serviceError(CodePermissionDenied, "installed image content changed")
	}
	return nil
}

func copyContext(ctx context.Context, destination io.Writer, source io.Reader) (int64, error) {
	buffer := make([]byte, 1024*1024)
	var total int64
	for {
		if err := ctx.Err(); err != nil {
			return total, contextErrorMessage(err)
		}
		read, readErr := source.Read(buffer)
		if read > 0 {
			written, writeErr := destination.Write(buffer[:read])
			total += int64(written)
			if writeErr != nil {
				return total, serviceError(CodeUnavailable, "copy image: %v", writeErr)
			}
			if written != read {
				return total, serviceError(CodeUnavailable, "copy image: %v", io.ErrShortWrite)
			}
		}
		if errors.Is(readErr, io.EOF) {
			return total, nil
		}
		if readErr != nil {
			return total, serviceError(CodeUnavailable, "read image: %v", readErr)
		}
	}
}

func ensureJSONEOF(decoder *json.Decoder) error {
	var extra any
	if err := decoder.Decode(&extra); errors.Is(err, io.EOF) {
		return nil
	} else if err != nil {
		return err
	}
	return errors.New("multiple JSON values")
}

func deriveHCSID(sid, vmID string) string {
	digest := sha256.Sum256([]byte(strings.ToUpper(sid) + "\x00" + vmID))
	digest[6] = (digest[6] & 0x0f) | 0x50
	digest[8] = (digest[8] & 0x3f) | 0x80
	value := hex.EncodeToString(digest[:16])
	return value[0:8] + "-" + value[8:12] + "-" + value[12:16] + "-" + value[16:20] + "-" + value[20:32]
}

func deriveOwner(sid string) string {
	digest := sha256.Sum256([]byte(strings.ToUpper(sid)))
	return "grok-desktop-" + hex.EncodeToString(digest[:8])
}

func hcsServiceError(operation string, err error) error {
	if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return contextErrorMessage(err)
	}
	var native *hcsapi.Error
	if errors.As(err, &native) {
		switch native.Code {
		case 0x8037010e, 0xc037010e:
			return serviceError(CodeNotFound, "%s: HCS system not found", operation)
		case 0x80370105, 0xc0370105, 0x8037010f, 0xc037010f, 0x80370110:
			return serviceError(CodeConflict, "%s: HCS state conflict", operation)
		case 0x8037011b:
			return serviceError(CodePermissionDenied, "%s: HCS access denied", operation)
		}
		return serviceError(CodeUnavailable, "%s: HCS error 0x%08x", operation, native.Code)
	}
	return serviceError(CodeUnavailable, "%s: %v", operation, err)
}

func pathServiceError(operation string, err error) error {
	if errors.Is(err, os.ErrNotExist) {
		return serviceError(CodeNotFound, "%s: path does not exist", operation)
	}
	if errors.Is(err, os.ErrPermission) {
		return serviceError(CodePermissionDenied, "%s: access denied", operation)
	}
	return serviceError(CodePermissionDenied, "%s: path validation failed", operation)
}

func hcsNotFound(err error) bool {
	var native *hcsapi.Error
	return errors.As(err, &native) && (native.Code == 0x8037010e || native.Code == 0xc037010e)
}
