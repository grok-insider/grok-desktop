//go:build windows

package hcsapi

import (
	"reflect"
	"testing"
	"unsafe"

	"golang.org/x/sys/windows"
)

func TestNativeHandleABI(t *testing.T) {
	if unsafe.Sizeof(windows.Handle(0)) != unsafe.Sizeof(uintptr(0)) {
		t.Fatalf("Windows HANDLE size %d differs from uintptr size %d", unsafe.Sizeof(windows.Handle(0)), unsafe.Sizeof(uintptr(0)))
	}
	gotProcedures := make([]string, 0, len(requiredProcedures))
	for _, procedure := range requiredProcedures {
		gotProcedures = append(gotProcedures, procedure.Name)
	}
	wantProcedures := []string{
		"HcsCreateOperation", "HcsCloseOperation", "HcsCancelOperation", "HcsWaitForOperationResult",
		"HcsGetServiceProperties", "HcsEnumerateComputeSystems", "HcsCreateComputeSystem", "HcsOpenComputeSystem",
		"HcsCloseComputeSystem", "HcsStartComputeSystem", "HcsShutDownComputeSystem", "HcsTerminateComputeSystem",
		"HcsGrantVmAccess", "HcsRevokeVmAccess",
	}
	if !reflect.DeepEqual(gotProcedures, wantProcedures) {
		t.Fatalf("unexpected Compute Core procedure bindings: %#v", gotProcedures)
	}
	if unsafe.Sizeof(windowsFileIDInfoForTest{}) != 24 {
		t.Fatalf("FILE_ID_INFO ABI size = %d, want 24", unsafe.Sizeof(windowsFileIDInfoForTest{}))
	}
}

type windowsFileIDInfoForTest struct {
	VolumeSerialNumber uint64
	FileID             [16]byte
}

func TestHRESULTFailureABI(t *testing.T) {
	if !failed(uintptr(uint32(0x80370114))) {
		t.Fatal("HCS failure HRESULT was treated as success")
	}
	if failed(0) {
		t.Fatal("S_OK was treated as failure")
	}
}
