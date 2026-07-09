//go:build windows

package tenant

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"golang.org/x/sys/windows"
)

func TestSecureServiceStorageRootCreatesProtectedPolicy(t *testing.T) {
	root := filepath.Join(t.TempDir(), "service-root")
	sid := currentTestSID(t)
	policy := testOwnedPolicy(baseStorageSDDL, sid)
	t.Cleanup(func() { restoreTestDirectoryAccess(root, sid) })

	setup, _, err := openManagedStorageRoot(root, policy)
	if err != nil {
		t.Fatalf("openManagedStorageRoot: %v", err)
	}
	setup.close()
	assertDirectoryPolicy(t, root, policy)
}

func TestManagedServiceStorageRootSecuresProductParent(t *testing.T) {
	parent := filepath.Join(t.TempDir(), "Grok Desktop")
	root := filepath.Join(parent, "VM Service")
	sid := currentTestSID(t)
	policy := testOwnedPolicy(baseStorageSDDL, sid)
	t.Cleanup(func() {
		restoreTestDirectoryAccess(root, sid)
		restoreTestDirectoryAccess(parent, sid)
	})

	setup, _, err := openManagedStorageRootWithDepth(root, policy, managedStorageRootDepth(root))
	if err != nil {
		t.Fatalf("openManagedStorageRootWithDepth: %v", err)
	}
	assertDirectoryPolicy(t, parent, policy)
	assertDirectoryPolicy(t, root, policy)

	moved := parent + "-moved"
	if err := os.Rename(parent, moved); err == nil {
		setup.close()
		_ = os.Rename(moved, parent)
		t.Fatal("managed product parent was renamed while setup handles were open")
	}
	setup.close()
}

func TestManagedStorageRootDepth(t *testing.T) {
	for _, test := range []struct {
		root string
		want int
	}{
		{root: `C:\ProgramData\Grok Desktop\VM Service`, want: 2},
		{root: `C:\ProgramData\Other Product\VM Service`, want: 1},
		{root: `C:\ProgramData\Grok Desktop\Other Service`, want: 1},
	} {
		if got := managedStorageRootDepth(test.root); got != test.want {
			t.Errorf("managedStorageRootDepth(%q) = %d, want %d", test.root, got, test.want)
		}
	}
}

func TestSecureServiceStorageRootRepairsExistingDACL(t *testing.T) {
	root := filepath.Join(t.TempDir(), "service-root")
	if err := os.Mkdir(root, 0o700); err != nil {
		t.Fatal(err)
	}
	applyTestDirectoryPolicy(t, root, "D:P(A;OICI;FA;;;WD)")
	sid := currentTestSID(t)
	policy := testOwnedPolicy(baseStorageSDDL, sid)
	t.Cleanup(func() { restoreTestDirectoryAccess(root, sid) })

	setup, _, err := openManagedStorageRoot(root, policy)
	if err != nil {
		t.Fatalf("openManagedStorageRoot: %v", err)
	}
	setup.close()
	assertDirectoryPolicy(t, root, policy)
}

func TestStorageSetupHandleBlocksDirectorySubstitution(t *testing.T) {
	root := filepath.Join(t.TempDir(), "service-root")
	sid := currentTestSID(t)
	policy := testOwnedPolicy(baseStorageSDDL, sid)
	setup, _, err := openManagedStorageRoot(root, policy)
	if err != nil {
		t.Fatalf("openManagedStorageRoot: %v", err)
	}
	moved := root + "-moved"
	if err := os.Rename(root, moved); err == nil {
		setup.close()
		_ = os.Rename(moved, root)
		t.Fatal("service root was renamed while its validated setup handle was open")
	}
	setup.close()
	if err := os.Rename(root, moved); err != nil {
		t.Fatalf("rename after closing setup handle: %v", err)
	}
	if err := os.Rename(moved, root); err != nil {
		t.Fatalf("restore service root: %v", err)
	}
	t.Cleanup(func() { restoreTestDirectoryAccess(root, sid) })
}

func TestSecureTenantStorageAppliesScopedPolicies(t *testing.T) {
	dataRoot := filepath.Join(t.TempDir(), "service-root")
	sid := currentTestSID(t)
	roots, err := DeriveStorageRoots(dataRoot, sid)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		for _, path := range []string{
			filepath.Join(roots.ImageRoot, ".vm-service", "installed"),
			filepath.Join(roots.ImageRoot, ".vm-service", "vms"),
			filepath.Join(roots.ImageRoot, ".vm-service"),
			roots.StagingRoot, roots.WorkspaceRoot, roots.ImageRoot, roots.TenantRoot,
			filepath.Dir(roots.TenantRoot), dataRoot,
		} {
			restoreTestDirectoryAccess(path, sid)
		}
	})

	basePolicy := testOwnedPolicy(baseStorageSDDL, sid)
	tenantPolicy := testOwnedPolicy(formatTestSDDL(serviceStorageSDDL, sid), sid)
	writablePolicy := testOwnedPolicy(formatTestSDDL(writableStorageSDDL, sid), sid)
	if err := secureTenantStorageWithPolicies(roots, basePolicy, tenantPolicy, writablePolicy, 1); err != nil {
		t.Fatalf("secureTenantStorage: %v", err)
	}
	assertDirectoryPolicy(t, dataRoot, basePolicy)
	assertDirectoryPolicy(t, filepath.Dir(roots.TenantRoot), basePolicy)
	assertDirectoryPolicy(t, roots.TenantRoot, tenantPolicy)
	assertDirectoryPolicy(t, roots.ImageRoot, tenantPolicy)
	assertDirectoryPolicy(t, filepath.Join(roots.ImageRoot, ".vm-service"), tenantPolicy)
	assertDirectoryPolicy(t, filepath.Join(roots.ImageRoot, ".vm-service", "installed"), tenantPolicy)
	assertDirectoryPolicy(t, filepath.Join(roots.ImageRoot, ".vm-service", "vms"), tenantPolicy)
	assertDirectoryPolicy(t, roots.StagingRoot, writablePolicy)
	assertDirectoryPolicy(t, roots.WorkspaceRoot, writablePolicy)
}

func TestProductionStoragePoliciesAreOwnedByLocalSystem(t *testing.T) {
	system, err := windows.CreateWellKnownSid(windows.WinLocalSystemSid)
	if err != nil {
		t.Fatal(err)
	}
	for _, sddl := range []string{
		baseStorageSDDL,
		formatTestSDDL(serviceStorageSDDL, currentTestSID(t)),
		formatTestSDDL(writableStorageSDDL, currentTestSID(t)),
	} {
		descriptor, err := windows.SecurityDescriptorFromString(sddl)
		if err != nil {
			t.Fatal(err)
		}
		owner, _, err := descriptor.Owner()
		if err != nil || owner == nil || !owner.Equals(system) {
			t.Fatalf("production storage owner = %v, %v; want LocalSystem", owner, err)
		}
		group, _, err := descriptor.Group()
		if err != nil || group == nil || !group.Equals(system) {
			t.Fatalf("production storage group = %v, %v; want LocalSystem", group, err)
		}
	}
}

func TestSecureServiceStorageRootRejectsFileAndReparseComponents(t *testing.T) {
	t.Run("file", func(t *testing.T) {
		root := t.TempDir()
		file := filepath.Join(root, "not-a-directory")
		if err := os.WriteFile(file, []byte("file"), 0o600); err != nil {
			t.Fatal(err)
		}
		policy := testOwnedPolicy(baseStorageSDDL, currentTestSID(t))
		if _, _, err := openManagedStorageRoot(filepath.Join(file, "service-root"), policy); err == nil {
			t.Fatal("storage setup accepted a file path component")
		}
	})

	t.Run("reparse", func(t *testing.T) {
		root := t.TempDir()
		target := filepath.Join(root, "target")
		if err := os.Mkdir(target, 0o700); err != nil {
			t.Fatal(err)
		}
		link := filepath.Join(root, "redirect")
		if err := os.Symlink(target, link); err != nil {
			t.Skipf("creating a Windows directory symlink requires Developer Mode or elevation: %v", err)
		}
		policy := testOwnedPolicy(baseStorageSDDL, currentTestSID(t))
		if _, _, err := openManagedStorageRoot(filepath.Join(link, "service-root"), policy); err == nil {
			t.Fatal("storage setup followed a reparse-point ancestor")
		}
	})
}

func TestSplitLocalStoragePathRejectsRemoteAndReservedPaths(t *testing.T) {
	for _, root := range []string{
		`\\server\share\Grok Desktop\VM Service`,
		`C:\`,
		`C:\ProgramData\CON\VM Service`,
		`C:\ProgramData\Grok Desktop.\VM Service`,
	} {
		if _, _, err := splitLocalStoragePath(root); err == nil {
			t.Fatalf("splitLocalStoragePath(%q) accepted an unsafe root", root)
		}
	}
}

func assertDirectoryPolicy(t *testing.T, path, expectedSDDL string) {
	t.Helper()
	actual, err := windows.GetNamedSecurityInfo(
		path,
		windows.SE_FILE_OBJECT,
		windows.OWNER_SECURITY_INFORMATION|windows.GROUP_SECURITY_INFORMATION|windows.DACL_SECURITY_INFORMATION,
	)
	if err != nil {
		t.Fatalf("GetNamedSecurityInfo(%q): %v", path, err)
	}
	control, _, err := actual.Control()
	if err != nil {
		t.Fatal(err)
	}
	if control&windows.SE_DACL_PROTECTED == 0 {
		t.Fatalf("directory DACL is not protected: %s", actual.String())
	}
	expected, err := windows.SecurityDescriptorFromString(expectedSDDL)
	if err != nil {
		t.Fatal(err)
	}
	owner, _, err := actual.Owner()
	if err != nil {
		t.Fatal(err)
	}
	expectedOwner, _, err := expected.Owner()
	if err != nil {
		t.Fatal(err)
	}
	if owner == nil || expectedOwner == nil || !owner.Equals(expectedOwner) {
		t.Fatalf("directory owner = %v, want %v", owner, expectedOwner)
	}
	group, _, err := actual.Group()
	if err != nil {
		t.Fatal(err)
	}
	expectedGroup, _, err := expected.Group()
	if err != nil {
		t.Fatal(err)
	}
	if group == nil || expectedGroup == nil || !group.Equals(expectedGroup) {
		t.Fatalf("directory group = %v, want %v", group, expectedGroup)
	}
	actualACL, _, err := actual.DACL()
	if err != nil {
		t.Fatal(err)
	}
	expectedACL, _, err := expected.DACL()
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(storageACLBytes(actualACL), storageACLBytes(expectedACL)) {
		t.Fatalf("directory DACL = %s, want %s", actual.String(), expected.String())
	}
}

func currentTestSID(t *testing.T) string {
	t.Helper()
	user, err := windows.GetCurrentProcessToken().GetTokenUser()
	if err != nil {
		t.Fatal(err)
	}
	return user.User.Sid.String()
}

func applyTestDirectoryPolicy(t *testing.T, path, sddl string) {
	t.Helper()
	descriptor, err := windows.SecurityDescriptorFromString(sddl)
	if err != nil {
		t.Fatal(err)
	}
	dacl, _, err := descriptor.DACL()
	if err != nil {
		t.Fatal(err)
	}
	if err := windows.SetNamedSecurityInfo(
		path,
		windows.SE_FILE_OBJECT,
		windows.DACL_SECURITY_INFORMATION|windows.PROTECTED_DACL_SECURITY_INFORMATION,
		nil,
		nil,
		dacl,
		nil,
	); err != nil {
		t.Fatal(err)
	}
}

func restoreTestDirectoryAccess(path, sid string) {
	if _, err := os.Lstat(path); err != nil {
		return
	}
	descriptor, err := windows.SecurityDescriptorFromString(formatTestSDDL("D:P(A;OICI;FA;;;%s)", sid))
	if err != nil {
		return
	}
	dacl, _, err := descriptor.DACL()
	if err != nil {
		return
	}
	_ = windows.SetNamedSecurityInfo(
		path,
		windows.SE_FILE_OBJECT,
		windows.DACL_SECURITY_INFORMATION|windows.PROTECTED_DACL_SECURITY_INFORMATION,
		nil,
		nil,
		dacl,
		nil,
	)
}

func formatTestSDDL(format, sid string) string {
	return fmt.Sprintf(format, sid)
}

func testOwnedPolicy(production, sid string) string {
	owned := strings.Replace(production, "O:SYG:SY", "O:"+sid+"G:"+sid, 1)
	return strings.ReplaceAll(owned, ";;;SY)", ";;;"+sid+")")
}
