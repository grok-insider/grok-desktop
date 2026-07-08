package runner

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path"
	"regexp"
	"sort"
	"strings"

	"github.com/grok-insider/grok-desktop/guest/runner/internal/strictjson"
	"github.com/grok-insider/grok-desktop/native/windows-vm-service/manifestverify"
	jsonschema "github.com/santhosh-tekuri/jsonschema/v5"
)

const (
	maxCatalogBytes = 4 << 20
	maxBundleFiles  = 1024
	maxBundleBytes  = 512 << 20
	maxFileBytes    = 64 << 20
	maxBundleDepth  = 16
)

var digestPattern = regexp.MustCompile(`^[0-9a-f]{64}$`)

type Catalog struct {
	Version  int            `json:"version"`
	Revision uint64         `json:"revision"`
	Bundles  []CatalogEntry `json:"bundles"`
}

type CatalogEntry struct {
	ID                  string        `json:"id"`
	Version             string        `json:"version"`
	RootIndex           int           `json:"rootIndex"`
	BundlePath          string        `json:"bundlePath"`
	ManifestPath        string        `json:"manifestPath"`
	ManifestSHA256      string        `json:"manifestSha256"`
	AllowedCapabilities []string      `json:"allowedCapabilities"`
	Files               []CatalogFile `json:"files"`
}

type CatalogFile struct {
	Path       string `json:"path"`
	SHA256     string `json:"sha256"`
	Size       int64  `json:"size"`
	Executable bool   `json:"executable"`
}

type VerifiedCatalog struct {
	Revision uint64
	Bundles  map[string]*VerifiedBundle
}

type VerifiedBundle struct {
	Entry        CatalogEntry
	Manifest     *manifestverify.Manifest
	Adapter      AdapterDescriptor
	ConfigSchema *jsonschema.Schema
	Dir          *SecureDir
}

func (catalog *VerifiedCatalog) Close() {
	if catalog == nil {
		return
	}
	for _, bundle := range catalog.Bundles {
		bundle.Dir.Close()
	}
}

type CatalogVerifier struct {
	policy Policy
	trust  TrustStore
	roots  []*SecureDir
}

func NewCatalogVerifier(policy Policy, trust TrustStore) (*CatalogVerifier, error) {
	verifier := &CatalogVerifier{policy: policy, trust: trust}
	for _, rootPath := range policy.ManifestRoots {
		root, err := OpenSecureRoot(rootPath, policy.BundleOwnerUID, true)
		if err != nil {
			verifier.Close()
			return nil, fmt.Errorf("open trusted manifest root: %w", err)
		}
		verifier.roots = append(verifier.roots, root)
	}
	return verifier, nil
}

func (verifier *CatalogVerifier) Close() {
	for _, root := range verifier.roots {
		root.Close()
	}
	verifier.roots = nil
}

func (verifier *CatalogVerifier) Verify(data []byte) (*VerifiedCatalog, error) {
	var catalog Catalog
	if err := strictjson.Decode(data, maxCatalogBytes, &catalog); err != nil {
		return nil, fmt.Errorf("invalid catalog: %w", err)
	}
	if catalog.Version != 1 || catalog.Revision == 0 || catalog.Bundles == nil || len(catalog.Bundles) > 64 {
		return nil, errors.New("catalog header is invalid")
	}
	verified := &VerifiedCatalog{Revision: catalog.Revision, Bundles: make(map[string]*VerifiedBundle, len(catalog.Bundles))}
	for _, entry := range catalog.Bundles {
		if _, duplicate := verified.Bundles[entry.ID]; duplicate {
			verified.Close()
			return nil, errors.New("catalog contains a duplicate integration")
		}
		bundle, err := verifier.verifyBundle(entry)
		if err != nil {
			verified.Close()
			return nil, fmt.Errorf("catalog bundle rejected: %w", err)
		}
		verified.Bundles[entry.ID] = bundle
	}
	return verified, nil
}

func (verifier *CatalogVerifier) verifyBundle(entry CatalogEntry) (*VerifiedBundle, error) {
	if entry.ID == "" || entry.Version == "" || entry.RootIndex < 0 || entry.RootIndex >= len(verifier.roots) ||
		len(entry.Files) == 0 || len(entry.Files) > maxBundleFiles || entry.AllowedCapabilities == nil ||
		!digestPattern.MatchString(entry.ManifestSHA256) {
		return nil, errors.New("catalog entry is incomplete or out of bounds")
	}
	if err := manifestverify.ValidateBundlePath(entry.BundlePath); err != nil {
		return nil, errors.New("bundle path is unsafe")
	}
	if err := manifestverify.ValidateBundlePath(entry.ManifestPath); err != nil {
		return nil, errors.New("manifest path is unsafe")
	}
	bundleDir, err := verifier.roots[entry.RootIndex].OpenDir(entry.BundlePath, true)
	if err != nil {
		return nil, err
	}
	fail := func(err error) (*VerifiedBundle, error) {
		bundleDir.Close()
		return nil, err
	}

	files := make(map[string]CatalogFile, len(entry.Files))
	var total int64
	for _, file := range entry.Files {
		if err := manifestverify.ValidateBundlePath(file.Path); err != nil || !digestPattern.MatchString(file.SHA256) ||
			file.Size < 1 || file.Size > maxFileBytes || strings.Count(file.Path, "/") >= maxBundleDepth {
			return fail(errors.New("catalog file record is invalid"))
		}
		if _, duplicate := files[file.Path]; duplicate {
			return fail(errors.New("catalog file path is duplicated"))
		}
		files[file.Path] = file
		total += file.Size
		if total > maxBundleBytes {
			return fail(errors.New("bundle exceeds total size limit"))
		}
	}

	discovered, directories, err := walkBundle(bundleDir, "", 0)
	if err != nil {
		return fail(err)
	}
	if len(discovered) != len(files) {
		return fail(errors.New("bundle inventory does not match catalog"))
	}
	for discoveredPath := range discovered {
		if _, declared := files[discoveredPath]; !declared {
			return fail(errors.New("bundle contains an undeclared file"))
		}
	}
	for _, directory := range directories {
		prefix := directory + "/"
		declared := false
		for filePath := range files {
			if strings.HasPrefix(filePath, prefix) {
				declared = true
				break
			}
		}
		if !declared {
			return fail(errors.New("bundle contains an undeclared empty directory"))
		}
	}

	contents := make(map[string][]byte, 3)
	for filePath, record := range files {
		file, info, err := bundleDir.OpenFile(filePath, maxFileBytes, true)
		if err != nil {
			return fail(err)
		}
		hasher := sha256.New()
		data, err := io.ReadAll(io.TeeReader(io.LimitReader(file, record.Size+1), hasher))
		file.Close()
		if err != nil || int64(len(data)) != record.Size || info.Size() != record.Size || hex.EncodeToString(hasher.Sum(nil)) != record.SHA256 {
			return fail(errors.New("bundle file content does not match catalog"))
		}
		executable := info.Mode().Perm()&0o111 != 0
		if executable != record.Executable {
			return fail(errors.New("bundle file execute mode does not match catalog"))
		}
		if filePath == entry.ManifestPath {
			contents["manifest"] = data
		}
	}
	manifestRecord, exists := files[entry.ManifestPath]
	if !exists || manifestRecord.SHA256 != entry.ManifestSHA256 || manifestRecord.Executable {
		return fail(errors.New("manifest catalog binding is invalid"))
	}

	allowed := make(map[string]struct{}, len(entry.AllowedCapabilities))
	for _, capability := range entry.AllowedCapabilities {
		if _, duplicate := allowed[capability]; duplicate {
			return fail(errors.New("catalog capability is duplicated"))
		}
		allowed[capability] = struct{}{}
	}
	manifest, err := manifestverify.Verify(contents["manifest"], manifestverify.Policy{
		SupportedProtocol:             protocolVersion,
		TrustedKeys:                   verifier.trust.Keys,
		AllowedCapabilities:           allowed,
		PublisherTrust:                verifier.trust.PublisherTrust,
		UnsignedDevelopmentPublishers: verifier.policy.unsignedPublishers(),
		AllowUnsignedDevelopment:      verifier.policy.AllowUnsignedDevelopment,
	})
	if err != nil {
		return fail(err)
	}
	if manifest.ID != entry.ID || manifest.Version != entry.Version {
		return fail(errors.New("manifest identity does not match catalog"))
	}
	if len(manifest.Permissions.Network.Outbound) != 0 || len(manifest.Permissions.Network.Listen) != 0 {
		return fail(errors.New("network permissions are unavailable in the utility guest"))
	}
	executables := map[string]struct{}{manifest.Entrypoint.Command: {}}
	for _, name := range manifest.Permissions.Process.Spawn {
		executables[path.Join("bin", name)] = struct{}{}
	}
	for filePath, record := range files {
		_, allowedExecutable := executables[filePath]
		if record.Executable != allowedExecutable {
			return fail(errors.New("executable inventory exceeds manifest spawn policy"))
		}
	}
	for executable := range executables {
		if record, exists := files[executable]; !exists || !record.Executable {
			return fail(errors.New("manifest executable is absent from catalog"))
		}
	}

	adapterData, err := readVerifiedFile(bundleDir, files, manifest.Entrypoint.Adapter, 64<<10)
	if err != nil {
		return fail(err)
	}
	adapter, err := parseAdapter(adapterData, verifier.policy)
	if err != nil {
		return fail(err)
	}
	configData, err := readVerifiedFile(bundleDir, files, manifest.ConfigSchema, 256<<10)
	if err != nil {
		return fail(err)
	}
	configSchema, err := compileSchema(configData, "mem:///config-schema.json")
	if err != nil {
		return fail(err)
	}
	expectedState := path.Join(verifier.policy.StateRoot, manifest.ID)
	for _, root := range manifest.Permissions.Filesystem.ReadWriteRoots {
		if root != expectedState {
			return fail(errors.New("writable root is outside integration state"))
		}
	}
	for _, root := range manifest.Permissions.Filesystem.ReadOnlyRoots {
		if root != verifier.policy.WorkspaceRoot {
			return fail(errors.New("read-only root is outside workspace policy"))
		}
	}
	return &VerifiedBundle{Entry: entry, Manifest: manifest, Adapter: adapter, ConfigSchema: configSchema, Dir: bundleDir}, nil
}

func (bundle *VerifiedBundle) ReverifyExecutable() (string, error) {
	if bundle == nil || bundle.Dir == nil || bundle.Manifest == nil {
		return "", errors.New("verified bundle is unavailable")
	}
	record, exists := catalogFiles(bundle.Entry.Files)[bundle.Manifest.Entrypoint.Command]
	if !exists || !record.Executable {
		return "", errors.New("entrypoint is not cataloged as executable")
	}
	file, info, err := bundle.Dir.OpenFile(record.Path, maxFileBytes, true)
	if err != nil {
		return "", err
	}
	hasher := sha256.New()
	read, err := io.Copy(hasher, io.LimitReader(file, record.Size+1))
	file.Close()
	if err != nil || read != record.Size || info.Size() != record.Size || hex.EncodeToString(hasher.Sum(nil)) != record.SHA256 {
		return "", errors.New("entrypoint changed after catalog verification")
	}
	return path.Join(bundle.Dir.Path(), record.Path), nil
}

func catalogFiles(records []CatalogFile) map[string]CatalogFile {
	files := make(map[string]CatalogFile, len(records))
	for _, record := range records {
		files[record.Path] = record
	}
	return files
}

func readVerifiedFile(directory *SecureDir, files map[string]CatalogFile, filePath string, maximum int64) ([]byte, error) {
	record, exists := files[filePath]
	if !exists || record.Executable || record.Size > maximum {
		return nil, errors.New("required metadata file is not safely cataloged")
	}
	file, info, err := directory.OpenFile(filePath, maximum, true)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	data, err := io.ReadAll(io.LimitReader(file, maximum+1))
	if err != nil || int64(len(data)) != record.Size || info.Size() != record.Size {
		return nil, errors.New("metadata file size changed after catalog verification")
	}
	digest := sha256.Sum256(data)
	if hex.EncodeToString(digest[:]) != record.SHA256 {
		return nil, errors.New("metadata file changed after catalog verification")
	}
	return data, nil
}

func walkBundle(directory *SecureDir, prefix string, depth int) (map[string]struct{}, []string, error) {
	if depth > maxBundleDepth {
		return nil, nil, errors.New("bundle directory depth exceeds limit")
	}
	entries, err := directory.ReadDirNames()
	if err != nil {
		return nil, nil, err
	}
	sort.Slice(entries, func(left, right int) bool { return entries[left].Name() < entries[right].Name() })
	files := make(map[string]struct{})
	var directories []string
	for _, entry := range entries {
		name := entry.Name()
		relative := name
		if prefix != "" {
			relative = prefix + "/" + name
		}
		if err := manifestverify.ValidateBundlePath(relative); err != nil || entry.Type()&os.ModeSymlink != 0 {
			return nil, nil, errors.New("bundle contains an unsafe directory entry")
		}
		if entry.IsDir() {
			child, err := directory.OpenDir(name, true)
			if err != nil {
				return nil, nil, err
			}
			childFiles, childDirectories, err := walkBundle(child, relative, depth+1)
			child.Close()
			if err != nil {
				return nil, nil, err
			}
			directories = append(directories, relative)
			directories = append(directories, childDirectories...)
			for childPath := range childFiles {
				files[childPath] = struct{}{}
			}
			continue
		}
		files[relative] = struct{}{}
		if len(files) > maxBundleFiles {
			return nil, nil, errors.New("bundle file count exceeds limit")
		}
	}
	return files, directories, nil
}
