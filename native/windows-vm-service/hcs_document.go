package vmservice

import (
	"encoding/json"
	"sort"
)

const (
	plan9Port               = 564
	plan9ShareReadOnly      = 0x1
	plan9ShareLinuxMetadata = 0x4
	// Linux AF_VSOCK ports map to Hyper-V service GUIDs using the documented
	// <8-hex-port>-facb-11e6-bd58-64006a7986d3 template.
	controlServiceID      = "00000fd2-facb-11e6-bd58-64006a7986d3" // 4050
	computerUseServiceID  = "00000fd3-facb-11e6-bd58-64006a7986d3" // 4051
	denyAllDescriptor     = "D:P(D;;GA;;;WD)"
	localSystemDescriptor = "D:P(A;;GA;;;SY)"
	primaryDiskController = "Primary disk"
)

var socketServiceIDs = map[SocketPurpose]string{
	SocketPurposeControl:       controlServiceID,
	SocketPurposeComputerUseV1: computerUseServiceID,
}

type hcsVersion struct {
	Major uint32 `json:"Major"`
	Minor uint32 `json:"Minor"`
}

type hcsBootEntry struct {
	DevicePath string `json:"DevicePath"`
	DiskNumber int32  `json:"DiskNumber"`
	DeviceType string `json:"DeviceType"`
}

type hcsMemory struct {
	Backing  string `json:"Backing"`
	SizeInMB uint64 `json:"SizeInMB"`
}

type hcsProcessor struct {
	Count uint32 `json:"Count"`
}

type hcsAttachment struct {
	Path string `json:"Path"`
	Type string `json:"Type"`
}

type hcsSCSI struct {
	Attachments map[string]hcsAttachment `json:"Attachments"`
}

type hcsPlan9Share struct {
	AccessName string `json:"AccessName"`
	Flags      uint32 `json:"Flags"`
	Name       string `json:"Name"`
	Path       string `json:"Path"`
	Port       int32  `json:"Port"`
}

type hcsPlan9 struct {
	Shares []hcsPlan9Share `json:"Shares"`
}

type hcsSocketService struct {
	AllowWildcardBinds        bool   `json:"AllowWildcardBinds"`
	BindSecurityDescriptor    string `json:"BindSecurityDescriptor"`
	ConnectSecurityDescriptor string `json:"ConnectSecurityDescriptor"`
	Disabled                  bool   `json:"Disabled"`
}

type hcsSocketConfig struct {
	DefaultBindSecurityDescriptor    string                      `json:"DefaultBindSecurityDescriptor"`
	DefaultConnectSecurityDescriptor string                      `json:"DefaultConnectSecurityDescriptor"`
	ServiceTable                     map[string]hcsSocketService `json:"ServiceTable"`
}

type hcsDocument struct {
	Owner                             string     `json:"Owner"`
	SchemaVersion                     hcsVersion `json:"SchemaVersion"`
	ShouldTerminateOnLastHandleClosed bool       `json:"ShouldTerminateOnLastHandleClosed"`
	VirtualMachine                    struct {
		Chipset struct {
			Uefi struct {
				BootThis hcsBootEntry `json:"BootThis"`
			} `json:"Uefi"`
		} `json:"Chipset"`
		ComputeTopology struct {
			Memory    hcsMemory    `json:"Memory"`
			Processor hcsProcessor `json:"Processor"`
		} `json:"ComputeTopology"`
		Devices struct {
			HvSocket struct {
				HvSocketConfig hcsSocketConfig `json:"HvSocketConfig"`
			} `json:"HvSocket"`
			Plan9 *hcsPlan9          `json:"Plan9,omitempty"`
			Scsi  map[string]hcsSCSI `json:"Scsi"`
		} `json:"Devices"`
	} `json:"VirtualMachine"`
}

type resolvedWorkspace struct {
	MountID string
	Path    string
}

func buildHCSDocument(vm *storedVM, diskPath, owner string, workspaces []resolvedWorkspace, purposes map[SocketPurpose]struct{}) ([]byte, error) {
	var document hcsDocument
	document.Owner = owner
	document.SchemaVersion = hcsVersion{Major: 2, Minor: 1}
	document.ShouldTerminateOnLastHandleClosed = false
	document.VirtualMachine.Chipset.Uefi.BootThis = hcsBootEntry{
		DevicePath: primaryDiskController,
		DiskNumber: 0,
		DeviceType: "ScsiDrive",
	}
	document.VirtualMachine.ComputeTopology.Memory = hcsMemory{Backing: "Virtual", SizeInMB: uint64(vm.MemoryMiB)}
	document.VirtualMachine.ComputeTopology.Processor = hcsProcessor{Count: uint32(vm.VCPUCount)}
	document.VirtualMachine.Devices.Scsi = map[string]hcsSCSI{
		primaryDiskController: {
			Attachments: map[string]hcsAttachment{"0": {Path: diskPath, Type: "VirtualDisk"}},
		},
	}

	sort.Slice(workspaces, func(i, j int) bool { return workspaces[i].MountID < workspaces[j].MountID })
	if len(workspaces) > 0 {
		document.VirtualMachine.Devices.Plan9 = &hcsPlan9{Shares: make([]hcsPlan9Share, 0, len(workspaces))}
	}
	for _, workspace := range workspaces {
		document.VirtualMachine.Devices.Plan9.Shares = append(document.VirtualMachine.Devices.Plan9.Shares, hcsPlan9Share{
			AccessName: workspace.MountID,
			Flags:      plan9ShareReadOnly | plan9ShareLinuxMetadata,
			Name:       workspace.MountID,
			Path:       workspace.Path,
			Port:       plan9Port,
		})
	}

	services := make(map[string]hcsSocketService, len(purposes))
	for purpose := range purposes {
		serviceID, ok := socketServiceIDs[purpose]
		if !ok {
			return nil, serviceError(CodeUnavailable, "socket purpose %q has no fixed Hyper-V socket service ID", purpose)
		}
		services[serviceID] = hcsSocketService{
			AllowWildcardBinds:        false,
			BindSecurityDescriptor:    localSystemDescriptor,
			ConnectSecurityDescriptor: localSystemDescriptor,
			Disabled:                  false,
		}
	}
	document.VirtualMachine.Devices.HvSocket.HvSocketConfig = hcsSocketConfig{
		DefaultBindSecurityDescriptor:    denyAllDescriptor,
		DefaultConnectSecurityDescriptor: denyAllDescriptor,
		ServiceTable:                     services,
	}
	return json.Marshal(document)
}
