package host

import (
	"context"
	"crypto/sha256"
	"encoding/json"

	vmservice "github.com/grok-insider/grok-desktop/native/windows-vm-service"
	"github.com/grok-insider/grok-desktop/native/windows-vm-service/transport"
)

type GetCapabilitiesPayload struct{}

type EnsureImagePayload struct {
	ImageID      string `json:"imageId"`
	RelativePath string `json:"relativePath"`
	SHA256       string `json:"sha256"`
	SizeBytes    int64  `json:"sizeBytes"`
}

type CreateVmPayload struct {
	VmID      string `json:"vmId"`
	ImageID   string `json:"imageId"`
	VCPUCount uint16 `json:"vcpuCount"`
	MemoryMiB uint32 `json:"memoryMiB"`
}

type VmPayload struct {
	VmID string `json:"vmId"`
}

type StopVmPayload struct {
	VmID string             `json:"vmId"`
	Mode vmservice.StopMode `json:"mode,omitempty"`
}

type AttachWorkspacePayload struct {
	VmID         string `json:"vmId"`
	MountID      string `json:"mountId"`
	RelativePath string `json:"relativePath"`
	ReadOnly     bool   `json:"readOnly"`
}

type OpenSocketPayload struct {
	VmID    string                  `json:"vmId"`
	Purpose vmservice.SocketPurpose `json:"purpose"`
}

type GuestControlPayload struct {
	VmID   string                       `json:"vmId"`
	Method vmservice.GuestControlMethod `json:"method"`
	Params json.RawMessage              `json:"params"`
}

type deleteResult struct {
	Deleted bool `json:"deleted"`
}

type decodedCall struct {
	canonicalPayload []byte
	invoke           func(context.Context) (any, error)
}

func decodeCall(
	service vmservice.Service,
	peer transport.PeerIdentity,
	request RequestEnvelope,
) (decodedCall, *protocolError) {
	identity := vmservice.RequestIdentity{RequestID: request.ID, UserSID: peer.UserSID}
	switch request.Operation {
	case vmservice.OperationGetCapabilities:
		payload, canonical, err := decodePayload[GetCapabilitiesPayload](request.Payload)
		if err != nil {
			return decodedCall{}, err
		}
		_ = payload
		return decodedCall{
			canonicalPayload: canonical,
			invoke: func(ctx context.Context) (any, error) {
				return service.GetCapabilities(ctx, vmservice.GetCapabilitiesRequest{Request: identity})
			},
		}, nil
	case vmservice.OperationEnsureImage:
		payload, canonical, err := decodePayload[EnsureImagePayload](request.Payload)
		if err != nil {
			return decodedCall{}, err
		}
		return decodedCall{
			canonicalPayload: canonical,
			invoke: func(ctx context.Context) (any, error) {
				return service.EnsureImage(ctx, vmservice.EnsureImageRequest{
					Request: identity, ImageID: payload.ImageID, RelativePath: payload.RelativePath,
					SHA256: payload.SHA256, SizeBytes: payload.SizeBytes,
				})
			},
		}, nil
	case vmservice.OperationCreateVM:
		payload, canonical, err := decodePayload[CreateVmPayload](request.Payload)
		if err != nil {
			return decodedCall{}, err
		}
		return decodedCall{
			canonicalPayload: canonical,
			invoke: func(ctx context.Context) (any, error) {
				return service.CreateVm(ctx, vmservice.CreateVmRequest{
					Request: identity, VmID: payload.VmID, ImageID: payload.ImageID,
					VCPUCount: payload.VCPUCount, MemoryMiB: payload.MemoryMiB,
				})
			},
		}, nil
	case vmservice.OperationStartVM:
		payload, canonical, err := decodePayload[VmPayload](request.Payload)
		if err != nil {
			return decodedCall{}, err
		}
		return decodedCall{
			canonicalPayload: canonical,
			invoke: func(ctx context.Context) (any, error) {
				return service.StartVm(ctx, vmservice.StartVmRequest{Request: identity, VmID: payload.VmID})
			},
		}, nil
	case vmservice.OperationStopVM:
		payload, canonical, err := decodePayload[StopVmPayload](request.Payload)
		if err != nil {
			return decodedCall{}, err
		}
		return decodedCall{
			canonicalPayload: canonical,
			invoke: func(ctx context.Context) (any, error) {
				return service.StopVm(ctx, vmservice.StopVmRequest{
					Request: identity, VmID: payload.VmID, Mode: payload.Mode,
				})
			},
		}, nil
	case vmservice.OperationDeleteVM:
		payload, canonical, err := decodePayload[VmPayload](request.Payload)
		if err != nil {
			return decodedCall{}, err
		}
		return decodedCall{
			canonicalPayload: canonical,
			invoke: func(ctx context.Context) (any, error) {
				if err := service.DeleteVm(ctx, vmservice.DeleteVmRequest{Request: identity, VmID: payload.VmID}); err != nil {
					return nil, err
				}
				return deleteResult{Deleted: true}, nil
			},
		}, nil
	case vmservice.OperationAttachWorkspace:
		payload, canonical, err := decodePayload[AttachWorkspacePayload](request.Payload)
		if err != nil {
			return decodedCall{}, err
		}
		return decodedCall{
			canonicalPayload: canonical,
			invoke: func(ctx context.Context) (any, error) {
				return service.AttachWorkspace(ctx, vmservice.AttachWorkspaceRequest{
					Request: identity, VmID: payload.VmID, MountID: payload.MountID,
					RelativePath: payload.RelativePath, ReadOnly: payload.ReadOnly,
				})
			},
		}, nil
	case vmservice.OperationGuestControl:
		payload, canonical, err := decodePayload[GuestControlPayload](request.Payload)
		if err != nil {
			return decodedCall{}, err
		}
		return decodedCall{
			canonicalPayload: canonical,
			invoke: func(ctx context.Context) (any, error) {
				if !peer.GuestControlQualified {
					return nil, &vmservice.Error{
						Code:    vmservice.CodePermissionDenied,
						Message: "guest control requires a qualified signed desktop process",
					}
				}
				return service.GuestControl(ctx, vmservice.GuestControlRequest{
					Request: identity, VmID: payload.VmID, OperationID: request.IdempotencyKey,
					Method: payload.Method, Params: append(json.RawMessage(nil), payload.Params...),
				})
			},
		}, nil
	case vmservice.OperationOpenSocket:
		payload, canonical, err := decodePayload[OpenSocketPayload](request.Payload)
		if err != nil {
			return decodedCall{}, err
		}
		return decodedCall{
			canonicalPayload: canonical,
			invoke: func(ctx context.Context) (any, error) {
				return service.OpenSocket(ctx, vmservice.OpenSocketRequest{
					Request: identity, VmID: payload.VmID, Purpose: payload.Purpose,
				})
			},
		}, nil
	default:
		return decodedCall{}, newProtocolError(errorUnknownOperation, "operation %q is not supported", request.Operation)
	}
}

func decodePayload[T any](raw json.RawMessage) (T, []byte, *protocolError) {
	var payload T
	if err := decodeStrictJSON(raw, &payload); err != nil {
		return payload, nil, newProtocolError(errorMalformedRequest, "operation payload is invalid: %v", err)
	}
	canonical, err := json.Marshal(payload)
	if err != nil {
		return payload, nil, newProtocolError(errorInternal, "cannot canonicalize operation payload: %v", err)
	}
	if len(canonical) == 0 {
		return payload, nil, newProtocolError(errorInternal, "canonical operation payload is empty")
	}
	return payload, canonical, nil
}

func operationDigest(operation vmservice.Operation, canonicalPayload []byte) [32]byte {
	input := make([]byte, 0, len(operation)+1+len(canonicalPayload))
	input = append(input, []byte(operation)...)
	input = append(input, 0)
	input = append(input, canonicalPayload...)
	return sha256Sum(input)
}

func sha256Sum(input []byte) [32]byte {
	return sha256.Sum256(input)
}
