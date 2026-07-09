//go:build windows

package vmservice

import (
	"encoding/json"
	"testing"
)

func TestWindowsHCSConfigurationSchemaABI(t *testing.T) {
	vm := &storedVM{VCPUCount: 4, MemoryMiB: 4096}
	document, err := buildHCSDocument(
		vm,
		`C:\ProgramData\Grok\vm\disk.vhdx`,
		"grok-desktop-test",
		[]resolvedWorkspace{{MountID: "source", Path: `C:\Users\owner\source`}},
		map[SocketPurpose]struct{}{SocketPurposeControl: {}},
	)
	if err != nil {
		t.Fatal(err)
	}
	var decoded struct {
		SchemaVersion  hcsVersion `json:"SchemaVersion"`
		VirtualMachine struct {
			Devices struct {
				HvSocket json.RawMessage    `json:"HvSocket"`
				Plan9    *hcsPlan9          `json:"Plan9"`
				Scsi     map[string]hcsSCSI `json:"Scsi"`
			} `json:"Devices"`
		} `json:"VirtualMachine"`
	}
	if err := json.Unmarshal(document, &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.SchemaVersion != (hcsVersion{Major: 2, Minor: 1}) {
		t.Fatalf("schema version = %#v", decoded.SchemaVersion)
	}
	if len(decoded.VirtualMachine.Devices.Scsi) != 1 || decoded.VirtualMachine.Devices.Plan9 == nil || len(decoded.VirtualMachine.Devices.Plan9.Shares) != 1 {
		t.Fatalf("unexpected fixed device schema: %s", document)
	}
	if decoded.VirtualMachine.Devices.Plan9.Shares[0].Flags != plan9ShareReadOnly|plan9ShareLinuxMetadata {
		t.Fatal("Windows HCS Plan9 share lost its read-only ABI flag")
	}
	if len(decoded.VirtualMachine.Devices.HvSocket) == 0 {
		t.Fatal("Windows HCS Hyper-V socket configuration is missing")
	}
}
