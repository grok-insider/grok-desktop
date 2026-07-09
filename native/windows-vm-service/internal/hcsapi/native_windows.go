//go:build windows

package hcsapi

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"sync"
	"time"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	genericAll          = 0x10000000
	maxResultDocument   = 4096
	maxServiceDocument  = 4 << 20
	hresultWaitTimeout  = 0x80070102
	hresultErrorTimeout = 0x800705b4
	hcsOperationTimeout = 0x80370118
)

var (
	computeCore = windows.NewLazySystemDLL("computecore.dll")

	procCreateOperation      = computeCore.NewProc("HcsCreateOperation")
	procCloseOperation       = computeCore.NewProc("HcsCloseOperation")
	procCancelOperation      = computeCore.NewProc("HcsCancelOperation")
	procWaitOperation        = computeCore.NewProc("HcsWaitForOperationResult")
	procGetServiceProperties = computeCore.NewProc("HcsGetServiceProperties")
	procEnumerateSystems     = computeCore.NewProc("HcsEnumerateComputeSystems")
	procCreateSystem         = computeCore.NewProc("HcsCreateComputeSystem")
	procOpenSystem           = computeCore.NewProc("HcsOpenComputeSystem")
	procCloseSystem          = computeCore.NewProc("HcsCloseComputeSystem")
	procStartSystem          = computeCore.NewProc("HcsStartComputeSystem")
	procShutdownSystem       = computeCore.NewProc("HcsShutDownComputeSystem")
	procTerminateSystem      = computeCore.NewProc("HcsTerminateComputeSystem")
	procGrantVMAccess        = computeCore.NewProc("HcsGrantVmAccess")
	procRevokeVMAccess       = computeCore.NewProc("HcsRevokeVmAccess")
	requiredProcedures       = []*windows.LazyProc{
		procCreateOperation, procCloseOperation, procCancelOperation, procWaitOperation,
		procGetServiceProperties, procEnumerateSystems, procCreateSystem, procOpenSystem,
		procCloseSystem, procStartSystem, procShutdownSystem, procTerminateSystem,
		procGrantVMAccess, procRevokeVMAccess,
	}
)

type NativeClient struct {
	loadOnce sync.Once
	loadErr  error
}

var _ Client = (*NativeClient)(nil)

func NewClient() *NativeClient { return &NativeClient{} }

func (c *NativeClient) Probe(ctx context.Context) error {
	if err := c.load(); err != nil {
		return err
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	query, err := windows.UTF16PtrFromString(`{"PropertyTypes":["Basic"]}`)
	if err != nil {
		return err
	}
	var result *uint16
	r1, _, _ := procGetServiceProperties.Call(uintptr(unsafe.Pointer(query)), uintptr(unsafe.Pointer(&result)))
	document := takeString(result)
	if failed(r1) {
		return nativeError("HcsGetServiceProperties", r1, document)
	}
	if strings.TrimSpace(document) == "" {
		return &Error{Operation: "HcsGetServiceProperties", Document: "empty service properties"}
	}
	if len(document) > maxServiceDocument || !json.Valid([]byte(document)) {
		return &Error{Operation: "HcsGetServiceProperties", Document: "invalid or oversized service properties"}
	}
	var properties map[string]json.RawMessage
	if err := json.Unmarshal([]byte(document), &properties); err != nil || properties["Properties"] == nil {
		return &Error{Operation: "HcsGetServiceProperties", Document: "missing service properties"}
	}
	return nil
}

func (c *NativeClient) Enumerate(ctx context.Context, owner string) ([]System, error) {
	query, err := json.Marshal(struct {
		Owners []string `json:"Owners"`
		Types  []string `json:"Types"`
	}{Owners: []string{owner}, Types: []string{"VirtualMachine"}})
	if err != nil {
		return nil, err
	}
	document, err := c.operation(ctx, "HcsEnumerateComputeSystems", func(operation uintptr) (uintptr, error) {
		value, conversionErr := windows.UTF16PtrFromString(string(query))
		if conversionErr != nil {
			return 0, conversionErr
		}
		r1, _, _ := procEnumerateSystems.Call(uintptr(unsafe.Pointer(value)), operation)
		return r1, nil
	})
	if err != nil {
		return nil, err
	}
	var systems []System
	if err := json.Unmarshal([]byte(document), &systems); err != nil {
		return nil, fmt.Errorf("decode HCS enumeration: %w", err)
	}
	return systems, nil
}

func (c *NativeClient) Create(ctx context.Context, id string, configuration []byte) error {
	if err := c.load(); err != nil {
		return err
	}
	idValue, err := windows.UTF16PtrFromString(id)
	if err != nil {
		return err
	}
	configurationValue, err := windows.UTF16PtrFromString(string(configuration))
	if err != nil {
		return err
	}
	var system uintptr
	_, err = c.operation(ctx, "HcsCreateComputeSystem", func(operation uintptr) (uintptr, error) {
		r1, _, _ := procCreateSystem.Call(
			uintptr(unsafe.Pointer(idValue)),
			uintptr(unsafe.Pointer(configurationValue)),
			operation,
			0,
			uintptr(unsafe.Pointer(&system)),
		)
		return r1, nil
	})
	if system != 0 {
		procCloseSystem.Call(system)
	}
	if err == nil && system == 0 {
		return &Error{Operation: "HcsCreateComputeSystem", Document: "HCS returned a null system handle"}
	}
	return err
}

func (c *NativeClient) Start(ctx context.Context, id string) error {
	return c.systemOperation(ctx, id, "HcsStartComputeSystem", procStartSystem)
}

func (c *NativeClient) Shutdown(ctx context.Context, id string) error {
	return c.systemOperation(ctx, id, "HcsShutDownComputeSystem", procShutdownSystem)
}

func (c *NativeClient) Terminate(ctx context.Context, id string) error {
	return c.systemOperation(ctx, id, "HcsTerminateComputeSystem", procTerminateSystem)
}

func (c *NativeClient) GrantVMAccess(ctx context.Context, id, path string) error {
	return c.fileAccess(ctx, "HcsGrantVmAccess", procGrantVMAccess, id, path)
}

func (c *NativeClient) RevokeVMAccess(ctx context.Context, id, path string) error {
	return c.fileAccess(ctx, "HcsRevokeVmAccess", procRevokeVMAccess, id, path)
}

func (c *NativeClient) load() error {
	c.loadOnce.Do(func() {
		for _, procedure := range requiredProcedures {
			if err := procedure.Find(); err != nil {
				c.loadErr = fmt.Errorf("required HCS procedure %s is unavailable: %w", procedure.Name, err)
				return
			}
		}
	})
	return c.loadErr
}

func (c *NativeClient) systemOperation(ctx context.Context, id, name string, procedure *windows.LazyProc) error {
	if err := c.load(); err != nil {
		return err
	}
	idValue, err := windows.UTF16PtrFromString(id)
	if err != nil {
		return err
	}
	var system uintptr
	r1, _, _ := procOpenSystem.Call(uintptr(unsafe.Pointer(idValue)), genericAll, uintptr(unsafe.Pointer(&system)))
	if failed(r1) {
		return nativeError("HcsOpenComputeSystem", r1, "")
	}
	if system == 0 {
		return &Error{Operation: "HcsOpenComputeSystem", Document: "HCS returned a null system handle"}
	}
	defer procCloseSystem.Call(system)

	_, err = c.operation(ctx, name, func(operation uintptr) (uintptr, error) {
		result, _, _ := procedure.Call(system, operation, 0)
		return result, nil
	})
	return err
}

func (c *NativeClient) operation(ctx context.Context, name string, start func(uintptr) (uintptr, error)) (string, error) {
	if err := c.load(); err != nil {
		return "", err
	}
	if err := ctx.Err(); err != nil {
		return "", err
	}
	operation, _, callErr := procCreateOperation.Call(0, 0)
	if operation == 0 {
		return "", fmt.Errorf("HcsCreateOperation: %w", callErr)
	}
	defer procCloseOperation.Call(operation)

	result, err := start(operation)
	if err != nil {
		return "", err
	}
	if failed(result) {
		return "", nativeError(name, result, "")
	}

	timeout := operationTimeout(ctx)
	var documentPointer *uint16
	waitResult, _, _ := procWaitOperation.Call(operation, uintptr(timeout), uintptr(unsafe.Pointer(&documentPointer)))
	document := takeString(documentPointer)
	if failed(waitResult) {
		code := uint32(waitResult)
		if code == hresultWaitTimeout || code == hresultErrorTimeout || code == hcsOperationTimeout || ctx.Err() != nil {
			procCancelOperation.Call(operation)
			if ctx.Err() != nil {
				return "", ctx.Err()
			}
		}
		if len(document) > maxServiceDocument {
			return "", &Error{Operation: name, Document: "HCS result document exceeds its size limit"}
		}
		return "", nativeError(name, waitResult, document)
	}
	if len(document) > maxServiceDocument {
		return "", &Error{Operation: name, Document: "HCS result document exceeds its size limit"}
	}
	return document, nil
}

func (c *NativeClient) fileAccess(ctx context.Context, name string, procedure *windows.LazyProc, id, path string) error {
	if err := c.load(); err != nil {
		return err
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	idValue, err := windows.UTF16PtrFromString(id)
	if err != nil {
		return err
	}
	pathValue, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return err
	}
	r1, _, _ := procedure.Call(uintptr(unsafe.Pointer(idValue)), uintptr(unsafe.Pointer(pathValue)))
	if failed(r1) {
		return nativeError(name, r1, "")
	}
	return nil
}

func operationTimeout(ctx context.Context) uint32 {
	timeout := 30 * time.Second
	if deadline, ok := ctx.Deadline(); ok {
		remaining := time.Until(deadline)
		if remaining < timeout {
			timeout = remaining
		}
	}
	if timeout < time.Millisecond {
		return 1
	}
	return uint32(timeout / time.Millisecond)
}

func failed(result uintptr) bool { return int32(uint32(result)) < 0 }

func nativeError(operation string, result uintptr, document string) error {
	document = strings.TrimSpace(document)
	if len(document) > maxResultDocument {
		document = document[:maxResultDocument]
	}
	return &Error{Operation: operation, Code: uint32(result), Document: document}
}

func takeString(value *uint16) string {
	if value == nil {
		return ""
	}
	result := windows.UTF16PtrToString(value)
	// ComputeCore.h assigns returned PWSTR values to the caller and the
	// documented HCS examples use an HLOCAL owner for these result strings.
	_, _ = windows.LocalFree(windows.Handle(unsafe.Pointer(value)))
	return result
}
