//go:build windows

package tenant

import (
	"bytes"
	"fmt"
	"path/filepath"
	"runtime"
	"strings"
	"unsafe"

	"golang.org/x/sys/windows"
)

const (
	baseStorageSDDL     = "O:SYG:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;;0x1200a0;;;AU)"
	serviceStorageSDDL  = "O:SYG:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;%s)"
	writableStorageSDDL = "O:SYG:SYD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1301bf;;;%s)"

	fileCreatedInformation = 2
	volumeNameDOS          = 0
)

var reservedStorageNames = map[string]struct{}{
	"CON": {}, "PRN": {}, "AUX": {}, "NUL": {},
	"COM1": {}, "COM2": {}, "COM3": {}, "COM4": {}, "COM5": {},
	"COM6": {}, "COM7": {}, "COM8": {}, "COM9": {},
	"LPT1": {}, "LPT2": {}, "LPT3": {}, "LPT4": {}, "LPT5": {},
	"LPT6": {}, "LPT7": {}, "LPT8": {}, "LPT9": {},
}

type storageDirectoryIdentity struct {
	volume uint32
	index  uint64
}

type securedStorageDirectory struct {
	handle   windows.Handle
	path     string
	identity storageDirectoryIdentity
}

type storageSetup struct {
	directories []*securedStorageDirectory
}

func secureTenantStorage(roots StorageRoots, sid string) error {
	return secureTenantStorageWithPolicies(
		roots,
		baseStorageSDDL,
		fmt.Sprintf(serviceStorageSDDL, sid),
		fmt.Sprintf(writableStorageSDDL, sid),
		managedStorageRootDepth(filepath.Dir(filepath.Dir(roots.TenantRoot))),
	)
}

func secureTenantStorageWithPolicies(
	roots StorageRoots,
	baseSDDL, tenantSDDL, writableSDDL string,
	managedRootDepth int,
) error {
	base := filepath.Dir(filepath.Dir(roots.TenantRoot))
	tenants := filepath.Dir(roots.TenantRoot)
	if filepath.Dir(tenants) != base || filepath.Dir(roots.ImageRoot) != roots.TenantRoot ||
		filepath.Dir(roots.StagingRoot) != roots.ImageRoot || filepath.Dir(roots.WorkspaceRoot) != roots.TenantRoot {
		return fmt.Errorf("tenant storage topology is invalid")
	}

	setup, baseDirectory, err := openManagedStorageRootWithDepth(base, baseSDDL, managedRootDepth)
	if err != nil {
		return err
	}
	defer setup.close()

	tenantsDirectory, err := setup.ensureManagedChild(baseDirectory, filepath.Base(tenants), baseSDDL)
	if err != nil {
		return err
	}
	tenantDirectory, err := setup.ensureManagedChild(tenantsDirectory, filepath.Base(roots.TenantRoot), tenantSDDL)
	if err != nil {
		return err
	}
	imageDirectory, err := setup.ensureManagedChild(tenantDirectory, filepath.Base(roots.ImageRoot), tenantSDDL)
	if err != nil {
		return err
	}
	metadataDirectory, err := setup.ensureManagedChild(imageDirectory, ".vm-service", tenantSDDL)
	if err != nil {
		return err
	}
	if _, err := setup.ensureManagedChild(metadataDirectory, "installed", tenantSDDL); err != nil {
		return err
	}
	if _, err := setup.ensureManagedChild(metadataDirectory, "vms", tenantSDDL); err != nil {
		return err
	}
	if _, err := setup.ensureManagedChild(imageDirectory, filepath.Base(roots.StagingRoot), writableSDDL); err != nil {
		return err
	}
	if _, err := setup.ensureManagedChild(tenantDirectory, filepath.Base(roots.WorkspaceRoot), writableSDDL); err != nil {
		return err
	}
	return setup.revalidate()
}

func secureServiceStorageRoot(root string) error {
	setup, _, err := openManagedStorageRootWithDepth(root, baseStorageSDDL, managedStorageRootDepth(root))
	if err != nil {
		return err
	}
	defer setup.close()
	return setup.revalidate()
}

func openManagedStorageRoot(root, sddl string) (*storageSetup, *securedStorageDirectory, error) {
	return openManagedStorageRootWithDepth(root, sddl, 1)
}

func openManagedStorageRootWithDepth(
	root, sddl string,
	managedDepth int,
) (*storageSetup, *securedStorageDirectory, error) {
	volumeRoot, components, err := splitLocalStoragePath(root)
	if err != nil {
		return nil, nil, err
	}
	if managedDepth < 1 || managedDepth > len(components) {
		return nil, nil, fmt.Errorf("service storage managed path depth is invalid")
	}
	setup := &storageSetup{}
	failed := true
	defer func() {
		if failed {
			setup.close()
		}
	}()
	parent, err := openStorageVolumeRoot(volumeRoot)
	if err != nil {
		return nil, nil, err
	}
	setup.directories = append(setup.directories, parent)

	baseDescriptor, err := storageSecurityDescriptor(sddl)
	if err != nil {
		return nil, nil, err
	}
	managedDescriptor, err := storageSecurityDescriptor(sddl)
	if err != nil {
		return nil, nil, err
	}
	for index, component := range components {
		managed := index >= len(components)-managedDepth
		descriptor := baseDescriptor
		if managed {
			descriptor = managedDescriptor
		}
		child, err := openOrCreateStorageDirectory(parent, component, descriptor, managed)
		if err != nil {
			return nil, nil, err
		}
		setup.directories = append(setup.directories, child)
		parent = child
	}
	failed = false
	return setup, parent, nil
}

func managedStorageRootDepth(root string) int {
	if strings.EqualFold(filepath.Base(root), "VM Service") &&
		strings.EqualFold(filepath.Base(filepath.Dir(root)), "Grok Desktop") {
		return 2
	}
	return 1
}

func (setup *storageSetup) ensureManagedChild(
	parent *securedStorageDirectory,
	name, sddl string,
) (*securedStorageDirectory, error) {
	if err := validateStorageComponent(name); err != nil {
		return nil, err
	}
	descriptor, err := storageSecurityDescriptor(sddl)
	if err != nil {
		return nil, err
	}
	child, err := openOrCreateStorageDirectory(parent, name, descriptor, true)
	if err != nil {
		return nil, err
	}
	setup.directories = append(setup.directories, child)
	return child, nil
}

func openStorageVolumeRoot(path string) (*securedStorageDirectory, error) {
	pathPointer, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return nil, fmt.Errorf("encode service storage volume: %w", err)
	}
	handle, err := windows.CreateFile(
		pathPointer,
		windows.FILE_READ_ATTRIBUTES|windows.FILE_TRAVERSE|windows.SYNCHRONIZE,
		windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE,
		nil,
		windows.OPEN_EXISTING,
		windows.FILE_FLAG_BACKUP_SEMANTICS|windows.FILE_FLAG_OPEN_REPARSE_POINT,
		0,
	)
	if err != nil {
		return nil, fmt.Errorf("open service storage volume: %w", err)
	}
	directory, err := validateStorageDirectoryHandle(handle, path)
	if err != nil {
		_ = windows.CloseHandle(handle)
		return nil, err
	}
	return directory, nil
}

func openOrCreateStorageDirectory(
	parent *securedStorageDirectory,
	name string,
	descriptor *windows.SECURITY_DESCRIPTOR,
	managed bool,
) (*securedStorageDirectory, error) {
	if err := validateStorageComponent(name); err != nil {
		return nil, err
	}
	objectName, err := windows.NewNTUnicodeString(name)
	if err != nil {
		return nil, fmt.Errorf("encode service storage directory: %w", err)
	}
	attributes := &windows.OBJECT_ATTRIBUTES{
		RootDirectory:      parent.handle,
		ObjectName:         objectName,
		Attributes:         windows.OBJ_CASE_INSENSITIVE | windows.OBJ_DONT_REPARSE,
		SecurityDescriptor: descriptor,
	}
	attributes.Length = uint32(unsafe.Sizeof(*attributes))
	access := uint32(windows.FILE_READ_ATTRIBUTES | windows.FILE_TRAVERSE | windows.READ_CONTROL | windows.SYNCHRONIZE)
	if managed {
		access |= windows.WRITE_DAC | windows.WRITE_OWNER
	}
	var handle windows.Handle
	var status windows.IO_STATUS_BLOCK
	err = windows.NtCreateFile(
		&handle,
		access,
		attributes,
		&status,
		nil,
		windows.FILE_ATTRIBUTE_DIRECTORY,
		windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE,
		windows.FILE_OPEN_IF,
		windows.FILE_DIRECTORY_FILE|windows.FILE_SYNCHRONOUS_IO_NONALERT|
			windows.FILE_OPEN_REPARSE_POINT|windows.FILE_OPEN_FOR_BACKUP_INTENT,
		0,
		0,
	)
	runtime.KeepAlive(descriptor)
	runtime.KeepAlive(objectName)
	if err != nil {
		return nil, fmt.Errorf("open or create service storage directory: %w", err)
	}
	expectedPath := filepath.Join(parent.path, name)
	directory, err := validateStorageDirectoryHandle(handle, expectedPath)
	if err != nil {
		_ = windows.CloseHandle(handle)
		return nil, err
	}
	if managed {
		if err := applyDirectoryPolicyHandle(handle, descriptor); err != nil {
			_ = windows.CloseHandle(handle)
			return nil, err
		}
	}
	if managed || status.Information == fileCreatedInformation {
		if err := validateProtectedDirectoryPolicy(handle, descriptor); err != nil {
			_ = windows.CloseHandle(handle)
			return nil, err
		}
	}
	return directory, nil
}

func validateStorageDirectoryHandle(handle windows.Handle, expectedPath string) (*securedStorageDirectory, error) {
	var information windows.ByHandleFileInformation
	if err := windows.GetFileInformationByHandle(handle, &information); err != nil {
		return nil, fmt.Errorf("inspect service storage directory: %w", err)
	}
	if information.FileAttributes&windows.FILE_ATTRIBUTE_DIRECTORY == 0 ||
		information.FileAttributes&windows.FILE_ATTRIBUTE_REPARSE_POINT != 0 {
		return nil, fmt.Errorf("service storage path is not a direct directory")
	}
	resolved, err := storageFinalPath(handle)
	if err != nil {
		return nil, err
	}
	canonicalExpected, err := storageLongPath(expectedPath)
	if err != nil {
		return nil, err
	}
	if !strings.EqualFold(filepath.Clean(resolved), filepath.Clean(canonicalExpected)) {
		return nil, fmt.Errorf("service storage directory resolved to an unexpected path")
	}
	return &securedStorageDirectory{
		handle: handle,
		path:   filepath.Clean(expectedPath),
		identity: storageDirectoryIdentity{
			volume: information.VolumeSerialNumber,
			index:  uint64(information.FileIndexHigh)<<32 | uint64(information.FileIndexLow),
		},
	}, nil
}

func applyDirectoryPolicyHandle(handle windows.Handle, descriptor *windows.SECURITY_DESCRIPTOR) error {
	owner, _, err := descriptor.Owner()
	if err != nil {
		return fmt.Errorf("read service storage owner policy: %w", err)
	}
	group, _, err := descriptor.Group()
	if err != nil {
		return fmt.Errorf("read service storage group policy: %w", err)
	}
	dacl, _, err := descriptor.DACL()
	if err != nil {
		return fmt.Errorf("read service storage policy: %w", err)
	}
	if err := windows.SetSecurityInfo(
		handle,
		windows.SE_FILE_OBJECT,
		windows.OWNER_SECURITY_INFORMATION|windows.GROUP_SECURITY_INFORMATION|
			windows.DACL_SECURITY_INFORMATION|windows.PROTECTED_DACL_SECURITY_INFORMATION,
		owner,
		group,
		dacl,
		nil,
	); err != nil {
		return fmt.Errorf("apply service storage policy: %w", err)
	}
	return nil
}

func validateProtectedDirectoryPolicy(handle windows.Handle, expected *windows.SECURITY_DESCRIPTOR) error {
	descriptor, err := windows.GetSecurityInfo(
		handle,
		windows.SE_FILE_OBJECT,
		windows.OWNER_SECURITY_INFORMATION|windows.GROUP_SECURITY_INFORMATION|windows.DACL_SECURITY_INFORMATION,
	)
	if err != nil {
		return fmt.Errorf("read applied service storage policy: %w", err)
	}
	control, _, err := descriptor.Control()
	if err != nil {
		return fmt.Errorf("inspect applied service storage policy: %w", err)
	}
	if control&windows.SE_DACL_PROTECTED == 0 {
		return fmt.Errorf("service storage policy is not protected from inheritance")
	}
	owner, _, err := descriptor.Owner()
	if err != nil {
		return fmt.Errorf("inspect applied service storage owner: %w", err)
	}
	expectedOwner, _, err := expected.Owner()
	if err != nil || owner == nil || expectedOwner == nil || !owner.Equals(expectedOwner) {
		return fmt.Errorf("service storage owner does not match policy")
	}
	group, _, err := descriptor.Group()
	if err != nil {
		return fmt.Errorf("inspect applied service storage group: %w", err)
	}
	expectedGroup, _, err := expected.Group()
	if err != nil || group == nil || expectedGroup == nil || !group.Equals(expectedGroup) {
		return fmt.Errorf("service storage group does not match policy")
	}
	dacl, _, err := descriptor.DACL()
	if err != nil {
		return fmt.Errorf("inspect applied service storage DACL: %w", err)
	}
	expectedDACL, _, err := expected.DACL()
	if err != nil || dacl == nil || expectedDACL == nil || !bytes.Equal(storageACLBytes(dacl), storageACLBytes(expectedDACL)) {
		return fmt.Errorf("service storage DACL does not match policy")
	}
	return nil
}

func storageACLBytes(acl *windows.ACL) []byte {
	header := unsafe.Slice((*byte)(unsafe.Pointer(acl)), 8)
	size := int(header[2]) | int(header[3])<<8
	return unsafe.Slice((*byte)(unsafe.Pointer(acl)), size)
}

func storageSecurityDescriptor(sddl string) (*windows.SECURITY_DESCRIPTOR, error) {
	descriptor, err := windows.SecurityDescriptorFromString(sddl)
	if err != nil {
		return nil, fmt.Errorf("construct service storage policy: %w", err)
	}
	return descriptor, nil
}

func (setup *storageSetup) revalidate() error {
	for _, directory := range setup.directories {
		validated, err := validateStorageDirectoryHandle(directory.handle, directory.path)
		if err != nil {
			return err
		}
		if validated.identity != directory.identity {
			return fmt.Errorf("service storage directory identity changed during setup")
		}
	}
	return nil
}

func (setup *storageSetup) close() {
	for index := len(setup.directories) - 1; index >= 0; index-- {
		_ = windows.CloseHandle(setup.directories[index].handle)
	}
	setup.directories = nil
}

func splitLocalStoragePath(root string) (string, []string, error) {
	if root == "" || !filepath.IsAbs(root) || len(root) > 32_000 {
		return "", nil, fmt.Errorf("service storage root must be a bounded absolute local path")
	}
	clean := filepath.Clean(root)
	volume := filepath.VolumeName(clean)
	if len(volume) != 2 || volume[1] != ':' || !isASCIIAlpha(volume[0]) {
		return "", nil, fmt.Errorf("service storage root must use a local drive-letter volume")
	}
	volumeRoot := strings.ToUpper(volume[:1]) + ":" + string(filepath.Separator)
	relative, err := filepath.Rel(volumeRoot, clean)
	if err != nil || relative == "." || relative == "" || filepath.IsAbs(relative) {
		return "", nil, fmt.Errorf("service storage root cannot be a volume root")
	}
	components := strings.Split(relative, string(filepath.Separator))
	for _, component := range components {
		if err := validateStorageComponent(component); err != nil {
			return "", nil, err
		}
	}
	return volumeRoot, components, nil
}

func validateStorageComponent(component string) error {
	if component == "" || component == "." || component == ".." || len([]rune(component)) > 240 ||
		strings.ContainsAny(component, ":\x00") || strings.TrimRight(component, " .") != component {
		return fmt.Errorf("service storage path contains an unsafe component")
	}
	base := strings.ToUpper(strings.SplitN(component, ".", 2)[0])
	if _, reserved := reservedStorageNames[base]; reserved {
		return fmt.Errorf("service storage path contains a reserved Windows name")
	}
	return nil
}

func storageFinalPath(handle windows.Handle) (string, error) {
	size, err := windows.GetFinalPathNameByHandle(handle, nil, 0, volumeNameDOS)
	if err != nil {
		return "", fmt.Errorf("resolve service storage directory: %w", err)
	}
	buffer := make([]uint16, size+1)
	written, err := windows.GetFinalPathNameByHandle(handle, &buffer[0], uint32(len(buffer)), volumeNameDOS)
	if err != nil {
		return "", fmt.Errorf("resolve service storage directory: %w", err)
	}
	if written == 0 || written >= uint32(len(buffer)) {
		return "", fmt.Errorf("resolved service storage path is truncated")
	}
	resolved := windows.UTF16ToString(buffer[:written])
	if strings.HasPrefix(resolved, `\\?\UNC\`) {
		return `\\` + resolved[len(`\\?\UNC\`):], nil
	}
	return strings.TrimPrefix(resolved, `\\?\`), nil
}

// storageLongPath normalizes valid 8.3 aliases before comparing a trusted
// component path with the canonical path returned for its already-open handle.
// Hosted Windows runners commonly expose their temporary root through such an
// alias; the handle identity and no-reparse checks remain the security boundary.
func storageLongPath(path string) (string, error) {
	pathPointer, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return "", fmt.Errorf("encode service storage path: %w", err)
	}
	size, err := windows.GetLongPathName(pathPointer, nil, 0)
	if err != nil {
		return "", fmt.Errorf("canonicalize service storage path: %w", err)
	}
	buffer := make([]uint16, size+1)
	written, err := windows.GetLongPathName(pathPointer, &buffer[0], uint32(len(buffer)))
	if err != nil {
		return "", fmt.Errorf("canonicalize service storage path: %w", err)
	}
	if written == 0 || written >= uint32(len(buffer)) {
		return "", fmt.Errorf("canonical service storage path is truncated")
	}
	return windows.UTF16ToString(buffer[:written]), nil
}

func isASCIIAlpha(value byte) bool {
	return value >= 'A' && value <= 'Z' || value >= 'a' && value <= 'z'
}
