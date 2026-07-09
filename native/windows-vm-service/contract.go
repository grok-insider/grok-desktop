package vmservice

import (
	"context"
	"encoding/json"
	"time"
)

const ContractVersion = "1.1.0"

// Operation is an allowlisted privileged operation. There is deliberately no
// generic command execution or caller-selected PowerShell operation.
type Operation string

const (
	OperationGetCapabilities Operation = "get_capabilities"
	OperationEnsureImage     Operation = "ensure_image"
	OperationCreateVM        Operation = "create_vm"
	OperationStartVM         Operation = "start_vm"
	OperationStopVM          Operation = "stop_vm"
	OperationDeleteVM        Operation = "delete_vm"
	OperationAttachWorkspace Operation = "attach_workspace"
	// OperationGuestControl proxies one bounded request through the
	// service-owned authenticated guest channel. It never exposes a socket or
	// accepts a caller-selected guest transport envelope.
	OperationGuestControl Operation = "guest_control"
	// OperationOpenSocket is retained for wire compatibility. Production
	// backends fail it closed and never return raw guest endpoints.
	OperationOpenSocket Operation = "open_socket"
)

type RequestIdentity struct {
	RequestID string `json:"requestId"`
	UserSID   string `json:"userSid"`
}

type GetCapabilitiesRequest struct {
	Request RequestIdentity `json:"request"`
}

type Capabilities struct {
	ContractVersion string          `json:"contractVersion"`
	Backend         string          `json:"backend"`
	Simulated       bool            `json:"simulated"`
	Available       bool            `json:"available"`
	HCSSchema       string          `json:"hcsSchema,omitempty"`
	Operations      []Operation     `json:"operations"`
	WorkspaceMode   string          `json:"workspaceMode"`
	SocketPurposes  []SocketPurpose `json:"socketPurposes"`
}

type EnsureImageRequest struct {
	Request      RequestIdentity `json:"request"`
	ImageID      string          `json:"imageId"`
	RelativePath string          `json:"relativePath"`
	SHA256       string          `json:"sha256"`
	SizeBytes    int64           `json:"sizeBytes"`
}

type Image struct {
	ID           string `json:"id"`
	RelativePath string `json:"relativePath"`
	SHA256       string `json:"sha256"`
	SizeBytes    int64  `json:"sizeBytes"`
}

type CreateVmRequest struct {
	Request   RequestIdentity `json:"request"`
	VmID      string          `json:"vmId"`
	ImageID   string          `json:"imageId"`
	VCPUCount uint16          `json:"vcpuCount"`
	MemoryMiB uint32          `json:"memoryMiB"`
}

type VmState string

const (
	VmStateCreated VmState = "created"
	VmStateRunning VmState = "running"
	VmStateStopped VmState = "stopped"
)

type Vm struct {
	ID         string                `json:"id"`
	ImageID    string                `json:"imageId"`
	VCPUCount  uint16                `json:"vcpuCount"`
	MemoryMiB  uint32                `json:"memoryMiB"`
	State      VmState               `json:"state"`
	Workspaces []WorkspaceAttachment `json:"workspaces"`
	UpdatedAt  time.Time             `json:"updatedAt"`
}

type StartVmRequest struct {
	Request RequestIdentity `json:"request"`
	VmID    string          `json:"vmId"`
}

type StopMode string

const (
	StopModeGraceful StopMode = "graceful"
	StopModeForce    StopMode = "force"
)

type StopVmRequest struct {
	Request RequestIdentity `json:"request"`
	VmID    string          `json:"vmId"`
	Mode    StopMode        `json:"mode"`
}

type DeleteVmRequest struct {
	Request RequestIdentity `json:"request"`
	VmID    string          `json:"vmId"`
}

type AttachWorkspaceRequest struct {
	Request      RequestIdentity `json:"request"`
	VmID         string          `json:"vmId"`
	MountID      string          `json:"mountId"`
	RelativePath string          `json:"relativePath"`
	ReadOnly     bool            `json:"readOnly"`
}

type WorkspaceAttachment struct {
	MountID      string `json:"mountId"`
	RelativePath string `json:"relativePath"`
	ReadOnly     bool   `json:"readOnly"`
}

type SocketPurpose string

const (
	SocketPurposeControl       SocketPurpose = "control"
	SocketPurposeComputerUseV1 SocketPurpose = "computer-use-v1"
)

type OpenSocketRequest struct {
	Request RequestIdentity `json:"request"`
	VmID    string          `json:"vmId"`
	Purpose SocketPurpose   `json:"purpose"`
}

type Socket struct {
	ID       string        `json:"id"`
	VmID     string        `json:"vmId"`
	Purpose  SocketPurpose `json:"purpose"`
	Endpoint string        `json:"endpoint"`
}

// GuestControlMethod is the closed guest-runner surface available through the
// privileged service. The guest independently validates the method and its
// method-specific parameters before dispatch.
type GuestControlMethod string

const (
	GuestControlRunnerHealth     GuestControlMethod = "runner.health"
	GuestControlCatalogApply     GuestControlMethod = "catalog.apply"
	GuestControlIntegrationStart GuestControlMethod = "integration.start"
	GuestControlIntegrationStop  GuestControlMethod = "integration.stop"
	GuestControlIntegrationCall  GuestControlMethod = "integration.call"
)

type GuestControlRequest struct {
	Request     RequestIdentity    `json:"request"`
	VmID        string             `json:"vmId"`
	OperationID string             `json:"operationId"`
	Method      GuestControlMethod `json:"method"`
	Params      json.RawMessage    `json:"params"`
}

type GuestControlResult struct {
	Response json.RawMessage `json:"response"`
}

// Service is the complete privileged host contract. A transport must authenticate
// its peer independently and pass the authenticated SID in every request.
type Service interface {
	GetCapabilities(context.Context, GetCapabilitiesRequest) (Capabilities, error)
	EnsureImage(context.Context, EnsureImageRequest) (Image, error)
	CreateVm(context.Context, CreateVmRequest) (Vm, error)
	StartVm(context.Context, StartVmRequest) (Vm, error)
	StopVm(context.Context, StopVmRequest) (Vm, error)
	DeleteVm(context.Context, DeleteVmRequest) error
	AttachWorkspace(context.Context, AttachWorkspaceRequest) (WorkspaceAttachment, error)
	GuestControl(context.Context, GuestControlRequest) (GuestControlResult, error)
	OpenSocket(context.Context, OpenSocketRequest) (Socket, error)
	Close(context.Context) error
}
