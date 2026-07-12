// Package linuxvmservice implements the narrow privileged Linux isolation broker
// contract (platform ADRs 0004–0007). It never exposes raw guest endpoints.
package linuxvmservice

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"time"
)

// ContractVersion is the wire contract for get_capabilities.
const ContractVersion = "1.1.0"

// Backend identifies the QEMU/KVM Linux host backend.
const Backend = "qemu-kvm"

// WorkspaceMode is the only allowed workspace attachment mode on Linux.
const WorkspaceMode = "read-only-virtio-9p"

// Operation is an allowlisted privileged operation.
type Operation string

const (
	OperationGetCapabilities Operation = "get_capabilities"
	OperationEnsureImage     Operation = "ensure_image"
	OperationCreateVM        Operation = "create_vm"
	OperationStartVM         Operation = "start_vm"
	OperationStopVM          Operation = "stop_vm"
	OperationDeleteVM        Operation = "delete_vm"
	OperationAttachWorkspace Operation = "attach_workspace"
	OperationGuestControl    Operation = "guest_control"
)

// ErrUnavailable is returned when the platform cannot provide isolation.
var ErrUnavailable = errors.New("linux isolation broker unavailable")

// ErrUnauthorized is returned when peer identity or PoP fails.
var ErrUnauthorized = errors.New("linux isolation broker unauthorized")

// ErrInvalid is returned for contract validation failures.
var ErrInvalid = errors.New("linux isolation broker invalid argument")

// ErrNotFound is returned when a resource is missing.
var ErrNotFound = errors.New("linux isolation broker not found")

// Capabilities is the static probe document.
type Capabilities struct {
	ContractVersion string      `json:"contractVersion"`
	Backend         string      `json:"backend"`
	Simulated       bool        `json:"simulated"`
	Available       bool        `json:"available"`
	Operations      []Operation `json:"operations"`
	WorkspaceMode   string      `json:"workspaceMode"`
	Reason          string      `json:"reason,omitempty"`
}

// Image is a catalog-selected guest image that passed digest verification.
type Image struct {
	ID           string `json:"id"`
	RelativePath string `json:"relativePath"`
	SHA256       string `json:"sha256"`
	SizeBytes    int64  `json:"sizeBytes"`
}

// VmState is the closed VM lifecycle state.
type VmState string

const (
	VmStateCreated VmState = "created"
	VmStateRunning VmState = "running"
	VmStateStopped VmState = "stopped"
)

// Vm is a utility VM record.
type Vm struct {
	ID         string                `json:"id"`
	ImageID    string                `json:"imageId"`
	VCPUCount  uint16                `json:"vcpuCount"`
	MemoryMiB  uint32                `json:"memoryMiB"`
	State      VmState               `json:"state"`
	Workspaces []WorkspaceAttachment `json:"workspaces"`
	BootID     string                `json:"bootId,omitempty"`
	UpdatedAt  time.Time             `json:"updatedAt"`
}

// WorkspaceAttachment is a read-only host path share.
type WorkspaceAttachment struct {
	RelativePath string `json:"relativePath"`
	GuestPath    string `json:"guestPath"`
}

// GuestControlRequest is one bounded service-mediated guest call.
type GuestControlRequest struct {
	VmID    string `json:"vmId"`
	Method  string `json:"method"`
	Payload []byte `json:"payload,omitempty"`
}

// GuestControlResponse is a non-secret control result.
type GuestControlResponse struct {
	Method string `json:"method"`
	Body   []byte `json:"body,omitempty"`
}

// PeerIdentity is authenticated at the unix socket accept boundary.
type PeerIdentity struct {
	UID            uint32
	PID            int32
	ExecutablePath string
}

// HypervisorProcess is a live guest hypervisor process owned by the broker.
type HypervisorProcess interface {
	// Kill terminates the process tree. Idempotent.
	Kill() error
	// Alive reports whether the process has not exited.
	Alive() bool
}

// HypervisorSpawn starts a closed-template hypervisor for one VM.
// Callers must not expose QMP/monitor sockets to unprivileged clients.
type HypervisorSpawn func(ctx context.Context, vm Vm, imageAbsolutePath string) (HypervisorProcess, error)

// Config configures storage roots and allowed daemon binaries.
type Config struct {
	// ImageRoot is the service-owned directory for verified images.
	ImageRoot string
	// AllowedDaemonPaths are exact paths that may call the broker.
	AllowedDaemonPaths []string
	// RequireKVM when true refuses Available unless /dev/kvm exists.
	RequireKVM bool
	// Now overrides the clock in tests.
	Now func() time.Time
	// KVMPresent overrides kvm detection in tests (nil = probe /dev/kvm).
	KVMPresent *bool
	// Spawn starts the hypervisor. When nil, LookPath("qemu-system-x86_64") is used.
	// Tests inject a fake spawn; production fails closed if QEMU is absent.
	Spawn HypervisorSpawn
	// QemuBinary overrides LookPath for the default spawn implementation.
	QemuBinary string
	// RunnerHealthHook, when set, is the only path that may answer runner.health
	// without a live guest dialer (test/lab harness). Production leaves it nil
	// and requires Spawn + Alive process before guest control.
	RunnerHealthHook func(vmID string) ([]byte, error)
	// GuestHealthDial, when set, is preferred for runner.health over the hook.
	GuestHealthDial func(ctx context.Context, vmID string, bootID string) ([]byte, error)
}

var idPattern = regexp.MustCompile(`^[a-z][a-z0-9.-]{0,62}$`)
var sha256Pattern = regexp.MustCompile(`^[a-f0-9]{64}$`)

// Service is the closed privileged surface.
type Service struct {
	mu        sync.Mutex
	config    Config
	images    map[string]Image
	vms       map[string]*Vm
	granted   map[string]bool // vmID -> guest control granted for this process lifetime after PoP
	processes map[string]HypervisorProcess
}

// NewService constructs a broker. ImageRoot must exist and be private.
func NewService(config Config) (*Service, error) {
	if strings.TrimSpace(config.ImageRoot) == "" {
		return nil, fmt.Errorf("%w: image root required", ErrInvalid)
	}
	root, err := filepath.Abs(config.ImageRoot)
	if err != nil {
		return nil, err
	}
	info, err := os.Stat(root)
	if err != nil || !info.IsDir() {
		return nil, fmt.Errorf("%w: image root must be a directory", ErrInvalid)
	}
	if len(config.AllowedDaemonPaths) == 0 {
		return nil, fmt.Errorf("%w: allowed daemon paths required", ErrInvalid)
	}
	cleaned := make([]string, 0, len(config.AllowedDaemonPaths))
	for _, p := range config.AllowedDaemonPaths {
		abs, err := filepath.Abs(p)
		if err != nil {
			return nil, err
		}
		cleaned = append(cleaned, abs)
	}
	config.ImageRoot = root
	config.AllowedDaemonPaths = cleaned
	if config.Now == nil {
		config.Now = func() time.Time { return time.Now().UTC() }
	}
	if config.Spawn == nil {
		config.Spawn = defaultQemuSpawn(config.QemuBinary)
	}
	return &Service{
		config:    config,
		images:    make(map[string]Image),
		vms:       make(map[string]*Vm),
		granted:   make(map[string]bool),
		processes: make(map[string]HypervisorProcess),
	}, nil
}

func (s *Service) kvmReady() bool {
	if s.config.KVMPresent != nil {
		return *s.config.KVMPresent
	}
	if !s.config.RequireKVM {
		return true
	}
	_, err := os.Stat("/dev/kvm")
	return err == nil
}

// AuthorizePeer checks peercred path identity against the allowlist.
func (s *Service) AuthorizePeer(peer PeerIdentity) error {
	if peer.PID <= 0 {
		return fmt.Errorf("%w: invalid pid", ErrUnauthorized)
	}
	exe := filepath.Clean(peer.ExecutablePath)
	for _, allowed := range s.config.AllowedDaemonPaths {
		if exe == allowed {
			return nil
		}
	}
	return fmt.Errorf("%w: daemon path not allowlisted", ErrUnauthorized)
}

// GetCapabilities returns static facts. GuestControl is listed only when KVM is ready.
func (s *Service) GetCapabilities(ctx context.Context, peer PeerIdentity) (Capabilities, error) {
	if err := ctx.Err(); err != nil {
		return Capabilities{}, err
	}
	if err := s.AuthorizePeer(peer); err != nil {
		return Capabilities{}, err
	}
	ops := []Operation{
		OperationGetCapabilities,
		OperationEnsureImage,
		OperationCreateVM,
		OperationStartVM,
		OperationStopVM,
		OperationDeleteVM,
		OperationAttachWorkspace,
	}
	available := s.kvmReady()
	reason := ""
	if !available {
		reason = "kvm_unavailable"
	} else {
		ops = append(ops, OperationGuestControl)
	}
	// Never advertise simulated isolation as Work-ready.
	return Capabilities{
		ContractVersion: ContractVersion,
		Backend:         Backend,
		Simulated:       false,
		Available:       available,
		Operations:      ops,
		WorkspaceMode:   WorkspaceMode,
		Reason:          reason,
	}, nil
}

// EnsureImage verifies size and sha256 of a file under ImageRoot.
func (s *Service) EnsureImage(ctx context.Context, peer PeerIdentity, imageID, relativePath, wantSHA string, sizeBytes int64) (Image, error) {
	if err := ctx.Err(); err != nil {
		return Image{}, err
	}
	if err := s.AuthorizePeer(peer); err != nil {
		return Image{}, err
	}
	if !s.kvmReady() {
		return Image{}, fmt.Errorf("%w: kvm unavailable", ErrUnavailable)
	}
	if !idPattern.MatchString(imageID) {
		return Image{}, fmt.Errorf("%w: invalid image id", ErrInvalid)
	}
	if !sha256Pattern.MatchString(wantSHA) {
		return Image{}, fmt.Errorf("%w: invalid sha256", ErrInvalid)
	}
	if sizeBytes <= 0 || sizeBytes > 128*1024*1024*1024 {
		return Image{}, fmt.Errorf("%w: invalid size", ErrInvalid)
	}
	rel := filepath.Clean(relativePath)
	if rel == "." || strings.HasPrefix(rel, "..") || filepath.IsAbs(rel) {
		return Image{}, fmt.Errorf("%w: invalid relative path", ErrInvalid)
	}
	full := filepath.Join(s.config.ImageRoot, rel)
	if !strings.HasPrefix(full, s.config.ImageRoot+string(os.PathSeparator)) && full != s.config.ImageRoot {
		return Image{}, fmt.Errorf("%w: path escapes image root", ErrInvalid)
	}
	data, err := os.ReadFile(full)
	if err != nil {
		return Image{}, fmt.Errorf("%w: read image: %v", ErrNotFound, err)
	}
	if int64(len(data)) != sizeBytes {
		return Image{}, fmt.Errorf("%w: size mismatch", ErrInvalid)
	}
	sum := sha256.Sum256(data)
	got := hex.EncodeToString(sum[:])
	if got != wantSHA {
		return Image{}, fmt.Errorf("%w: digest mismatch", ErrInvalid)
	}
	image := Image{ID: imageID, RelativePath: rel, SHA256: wantSHA, SizeBytes: sizeBytes}
	s.mu.Lock()
	s.images[imageID] = image
	s.mu.Unlock()
	return image, nil
}

// CreateVm records a VM from an ensured image.
func (s *Service) CreateVm(ctx context.Context, peer PeerIdentity, vmID, imageID string, vcpus uint16, memoryMiB uint32) (Vm, error) {
	if err := ctx.Err(); err != nil {
		return Vm{}, err
	}
	if err := s.AuthorizePeer(peer); err != nil {
		return Vm{}, err
	}
	if !s.kvmReady() {
		return Vm{}, fmt.Errorf("%w: kvm unavailable", ErrUnavailable)
	}
	if !idPattern.MatchString(vmID) || !idPattern.MatchString(imageID) {
		return Vm{}, fmt.Errorf("%w: invalid id", ErrInvalid)
	}
	if vcpus == 0 || vcpus > 16 || memoryMiB < 256 || memoryMiB > 32*1024 {
		return Vm{}, fmt.Errorf("%w: invalid resources", ErrInvalid)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.images[imageID]; !ok {
		return Vm{}, fmt.Errorf("%w: image not ensured", ErrNotFound)
	}
	if _, ok := s.vms[vmID]; ok {
		return Vm{}, fmt.Errorf("%w: vm exists", ErrInvalid)
	}
	vm := &Vm{
		ID:        vmID,
		ImageID:   imageID,
		VCPUCount: vcpus,
		MemoryMiB: memoryMiB,
		State:     VmStateCreated,
		UpdatedAt: s.config.Now(),
	}
	s.vms[vmID] = vm
	return *vm, nil
}

// StartVm spawns the closed hypervisor template then marks the VM running.
// Without a successful spawn, the VM remains non-running (fail closed).
func (s *Service) StartVm(ctx context.Context, peer PeerIdentity, vmID string) (Vm, error) {
	if err := ctx.Err(); err != nil {
		return Vm{}, err
	}
	if err := s.AuthorizePeer(peer); err != nil {
		return Vm{}, err
	}
	if !s.kvmReady() {
		return Vm{}, fmt.Errorf("%w: kvm unavailable", ErrUnavailable)
	}
	s.mu.Lock()
	vm, ok := s.vms[vmID]
	if !ok {
		s.mu.Unlock()
		return Vm{}, fmt.Errorf("%w: vm", ErrNotFound)
	}
	if vm.State == VmStateRunning {
		proc := s.processes[vmID]
		s.mu.Unlock()
		if proc == nil || !proc.Alive() {
			return Vm{}, fmt.Errorf("%w: hypervisor process missing", ErrUnavailable)
		}
		return *vm, nil
	}
	if vm.State != VmStateCreated && vm.State != VmStateStopped {
		s.mu.Unlock()
		return Vm{}, fmt.Errorf("%w: invalid state", ErrInvalid)
	}
	image, ok := s.images[vm.ImageID]
	if !ok {
		s.mu.Unlock()
		return Vm{}, fmt.Errorf("%w: image not ensured", ErrNotFound)
	}
	imagePath := filepath.Join(s.config.ImageRoot, image.RelativePath)
	vmCopy := *vm
	s.mu.Unlock()

	if s.config.Spawn == nil {
		return Vm{}, fmt.Errorf("%w: hypervisor spawn not configured", ErrUnavailable)
	}
	proc, err := s.config.Spawn(ctx, vmCopy, imagePath)
	if err != nil {
		return Vm{}, fmt.Errorf("%w: hypervisor spawn: %v", ErrUnavailable, err)
	}
	if proc == nil || !proc.Alive() {
		if proc != nil {
			_ = proc.Kill()
		}
		return Vm{}, fmt.Errorf("%w: hypervisor process not alive after spawn", ErrUnavailable)
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok = s.vms[vmID]
	if !ok {
		_ = proc.Kill()
		return Vm{}, fmt.Errorf("%w: vm", ErrNotFound)
	}
	boot := fmt.Sprintf("%x", sha256.Sum256([]byte(fmt.Sprintf("%s:%d", vmID, s.config.Now().UnixNano()))))
	vm.State = VmStateRunning
	vm.BootID = boot[:32]
	vm.UpdatedAt = s.config.Now()
	s.processes[vmID] = proc
	return *vm, nil
}

// StopVm stops a running VM, kills its hypervisor process, and clears grants.
func (s *Service) StopVm(ctx context.Context, peer PeerIdentity, vmID string) (Vm, error) {
	if err := ctx.Err(); err != nil {
		return Vm{}, err
	}
	if err := s.AuthorizePeer(peer); err != nil {
		return Vm{}, err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.vms[vmID]
	if !ok {
		return Vm{}, fmt.Errorf("%w: vm", ErrNotFound)
	}
	if proc := s.processes[vmID]; proc != nil {
		_ = proc.Kill()
		delete(s.processes, vmID)
	}
	vm.State = VmStateStopped
	vm.BootID = ""
	vm.UpdatedAt = s.config.Now()
	delete(s.granted, vmID)
	return *vm, nil
}

// DeleteVm removes a non-running VM.
func (s *Service) DeleteVm(ctx context.Context, peer PeerIdentity, vmID string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if err := s.AuthorizePeer(peer); err != nil {
		return err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.vms[vmID]
	if !ok {
		return fmt.Errorf("%w: vm", ErrNotFound)
	}
	if vm.State == VmStateRunning {
		return fmt.Errorf("%w: stop before delete", ErrInvalid)
	}
	delete(s.vms, vmID)
	delete(s.granted, vmID)
	return nil
}

// AttachWorkspace attaches a relative path under ImageRoot as RO (metadata only).
func (s *Service) AttachWorkspace(ctx context.Context, peer PeerIdentity, vmID, relativePath, guestPath string) (Vm, error) {
	if err := ctx.Err(); err != nil {
		return Vm{}, err
	}
	if err := s.AuthorizePeer(peer); err != nil {
		return Vm{}, err
	}
	rel := filepath.Clean(relativePath)
	if rel == "." || strings.HasPrefix(rel, "..") || filepath.IsAbs(rel) {
		return Vm{}, fmt.Errorf("%w: invalid relative path", ErrInvalid)
	}
	if !strings.HasPrefix(guestPath, "/run/grok-desktop/workspaces/") {
		return Vm{}, fmt.Errorf("%w: invalid guest path", ErrInvalid)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.vms[vmID]
	if !ok {
		return Vm{}, fmt.Errorf("%w: vm", ErrNotFound)
	}
	if vm.State == VmStateRunning {
		return Vm{}, fmt.Errorf("%w: cannot change attachments while running", ErrInvalid)
	}
	vm.Workspaces = append(vm.Workspaces, WorkspaceAttachment{RelativePath: rel, GuestPath: guestPath})
	vm.UpdatedAt = s.config.Now()
	return *vm, nil
}

// GrantGuestControl records PoP-backed grant for a running VM (daemon-side PoP).
func (s *Service) GrantGuestControl(ctx context.Context, peer PeerIdentity, vmID, proof string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if err := s.AuthorizePeer(peer); err != nil {
		return err
	}
	if len(proof) < 32 {
		return fmt.Errorf("%w: proof of possession required", ErrUnauthorized)
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	vm, ok := s.vms[vmID]
	if !ok {
		return fmt.Errorf("%w: vm", ErrNotFound)
	}
	if vm.State != VmStateRunning {
		return fmt.Errorf("%w: vm not running", ErrInvalid)
	}
	s.granted[vmID] = true
	return nil
}

// GuestControl proxies one allowlisted method when grant is present.
func (s *Service) GuestControl(ctx context.Context, peer PeerIdentity, req GuestControlRequest) (GuestControlResponse, error) {
	if err := ctx.Err(); err != nil {
		return GuestControlResponse{}, err
	}
	if err := s.AuthorizePeer(peer); err != nil {
		return GuestControlResponse{}, err
	}
	if !s.kvmReady() {
		return GuestControlResponse{}, fmt.Errorf("%w: kvm unavailable", ErrUnavailable)
	}
	if req.Method != "runner.health" {
		return GuestControlResponse{}, fmt.Errorf("%w: method not allowlisted", ErrInvalid)
	}
	s.mu.Lock()
	granted := s.granted[req.VmID]
	vm, ok := s.vms[req.VmID]
	s.mu.Unlock()
	if !ok {
		return GuestControlResponse{}, fmt.Errorf("%w: vm", ErrNotFound)
	}
	if !granted {
		return GuestControlResponse{}, fmt.Errorf("%w: guest control not granted", ErrUnauthorized)
	}
	if vm.State != VmStateRunning {
		return GuestControlResponse{}, fmt.Errorf("%w: vm not running", ErrInvalid)
	}
	proc := s.processes[req.VmID]
	bootID := vm.BootID
	if proc == nil || !proc.Alive() {
		return GuestControlResponse{}, fmt.Errorf("%w: hypervisor process not alive", ErrUnavailable)
	}

	var body []byte
	var err error
	switch {
	case s.config.GuestHealthDial != nil:
		body, err = s.config.GuestHealthDial(ctx, req.VmID, bootID)
	case s.config.RunnerHealthHook != nil:
		// Lab/test harness only: still requires a live hypervisor process above.
		body, err = s.config.RunnerHealthHook(req.VmID)
	default:
		return GuestControlResponse{}, fmt.Errorf("%w: guest health dial not configured", ErrUnavailable)
	}
	if err != nil {
		return GuestControlResponse{}, err
	}
	if len(body) == 0 {
		return GuestControlResponse{}, fmt.Errorf("%w: empty guest health response", ErrUnavailable)
	}
	return GuestControlResponse{Method: req.Method, Body: body}, nil
}

// defaultQemuSpawn returns a closed QEMU/KVM template with no general NIC.
func defaultQemuSpawn(qemuBinary string) HypervisorSpawn {
	return func(ctx context.Context, vm Vm, imageAbsolutePath string) (HypervisorProcess, error) {
		binary := qemuBinary
		if binary == "" {
			var err error
			binary, err = exec.LookPath("qemu-system-x86_64")
			if err != nil {
				return nil, fmt.Errorf("qemu-system-x86_64 not found: %w", err)
			}
		}
		if _, err := os.Stat(imageAbsolutePath); err != nil {
			return nil, fmt.Errorf("image path: %w", err)
		}
		// Closed template: no user networking, no QMP/monitor socket exposure.
		args := []string{
			"-enable-kvm",
			"-nographic",
			"-nodefaults",
			"-no-reboot",
			"-machine", "q35",
			"-cpu", "host",
			"-m", fmt.Sprintf("%d", vm.MemoryMiB),
			"-smp", fmt.Sprintf("%d", vm.VCPUCount),
			"-drive", fmt.Sprintf("file=%s,if=virtio,format=raw,readonly=on", imageAbsolutePath),
			"-device", "vhost-vsock-pci,guest-cid=3",
			"-serial", "none",
			"-monitor", "none",
		}
		cmd := exec.CommandContext(ctx, binary, args...)
		cmd.Stdout = nil
		cmd.Stderr = nil
		if err := cmd.Start(); err != nil {
			return nil, err
		}
		return &qemuProcess{cmd: cmd}, nil
	}
}

type qemuProcess struct {
	cmd *exec.Cmd
}

func (p *qemuProcess) Kill() error {
	if p.cmd.Process == nil {
		return nil
	}
	return p.cmd.Process.Kill()
}

func (p *qemuProcess) Alive() bool {
	if p.cmd.Process == nil {
		return false
	}
	// ProcessState set after Wait; nil means still running (or not waited).
	if p.cmd.ProcessState != nil {
		return !p.cmd.ProcessState.Exited()
	}
	// Non-blocking signal 0 check via FindProcess is racy; use ProcessState only.
	// After Start without Wait, treat as alive until Kill.
	return true
}
