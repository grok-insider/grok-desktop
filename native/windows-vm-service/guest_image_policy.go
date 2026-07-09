package vmservice

import (
	"bytes"
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"strings"
)

const (
	GuestImageCatalogRelativePath = "catalog/components.json"
	guestImageCatalogVersion      = 1
	guestImagePolicyStateVersion  = 1
	maxGuestImageCatalogBytes     = 1 << 20
	maxGuestImagePolicyStateBytes = 16 << 10
	maxOfficialGuestImages        = 16
	maxGuestCatalogSequence       = uint64(1<<53 - 1)
)

var (
	guestCatalogKeyIDPattern = regexp.MustCompile(`^[A-Za-z0-9._:-]{1,128}$`)
	guestImageVersionPattern = regexp.MustCompile(`^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$`)
	guestStagingNamePattern  = regexp.MustCompile(`^[a-z][a-z0-9.-]{0,120}\.vhdx$`)
)

// GuestImageCatalog is the signed release contract for official utility-VM
// images. The signature covers every field except Signature.Value.
type GuestImageCatalog struct {
	SchemaVersion uint32                `json:"schemaVersion"`
	Product       string                `json:"product"`
	Architecture  string                `json:"architecture"`
	Sequence      uint64                `json:"sequence"`
	Images        []OfficialGuestImage  `json:"images"`
	Signature     GuestCatalogSignature `json:"signature"`
}

type OfficialGuestImage struct {
	ID          string `json:"id"`
	Version     string `json:"version"`
	StagingName string `json:"stagingName"`
	SHA256      string `json:"sha256"`
	SizeBytes   int64  `json:"sizeBytes"`
}

type GuestCatalogSignature struct {
	Algorithm string `json:"algorithm"`
	KeyID     string `json:"keyId"`
	Value     string `json:"value"`
}

type guestCatalogSignatureIdentity struct {
	Algorithm string `json:"algorithm"`
	KeyID     string `json:"keyId"`
}

type guestImageCatalogSigningDocument struct {
	SchemaVersion uint32                        `json:"schemaVersion"`
	Product       string                        `json:"product"`
	Architecture  string                        `json:"architecture"`
	Sequence      uint64                        `json:"sequence"`
	Images        []OfficialGuestImage          `json:"images"`
	Signature     guestCatalogSignatureIdentity `json:"signature"`
}

type GuestImageTrust struct {
	keys map[string]ed25519.PublicKey
}

// GuestImagePolicy is immutable verified catalog state shared by tenant
// backends. Its fields are private so callers cannot manufacture trusted image
// metadata after verification.
type GuestImagePolicy struct {
	architecture  string
	sequence      uint64
	catalogSHA256 string
	signingKeyID  string
	images        map[string]OfficialGuestImage
}

type guestImagePolicyState struct {
	Version       uint32 `json:"version"`
	Architecture  string `json:"architecture"`
	Sequence      uint64 `json:"sequence"`
	CatalogSHA256 string `json:"catalogSha256"`
	SigningKeyID  string `json:"signingKeyId"`
}

// ParseGuestImageTrust decodes a linker-supplied key-id to raw Ed25519 public
// key map. It accepts public material only; private signing keys never enter the
// service process.
func ParseGuestImageTrust(encoded string) (GuestImageTrust, error) {
	data := []byte(encoded)
	if len(data) == 0 || len(data) > 64<<10 {
		return GuestImageTrust{}, fmt.Errorf("guest image trust is missing or oversized")
	}
	if err := rejectDuplicateJSONKeys(data); err != nil {
		return GuestImageTrust{}, fmt.Errorf("guest image trust is malformed")
	}
	var values map[string]string
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&values); err != nil || values == nil {
		return GuestImageTrust{}, fmt.Errorf("guest image trust is malformed")
	}
	if err := ensureJSONEOF(decoder); err != nil || len(values) == 0 || len(values) > 16 {
		return GuestImageTrust{}, fmt.Errorf("guest image trust must contain 1 to 16 keys")
	}
	keys := make(map[string]ed25519.PublicKey, len(values))
	for keyID, value := range values {
		if !guestCatalogKeyIDPattern.MatchString(keyID) {
			return GuestImageTrust{}, fmt.Errorf("guest image trust contains an invalid key identifier")
		}
		key, err := decodeCanonicalBase64(value, ed25519.PublicKeySize)
		if err != nil {
			return GuestImageTrust{}, fmt.Errorf("guest image trust contains an invalid Ed25519 public key")
		}
		keys[keyID] = ed25519.PublicKey(append([]byte(nil), key...))
	}
	return GuestImageTrust{keys: keys}, nil
}

// GuestArchitectureForRuntime maps Go's Windows architecture name to the
// architecture embedded in the signed component catalog.
func GuestArchitectureForRuntime(goarch string) (string, error) {
	switch goarch {
	case "amd64":
		return "x64", nil
	case "arm64":
		return "arm64", nil
	default:
		return "", fmt.Errorf("unsupported guest architecture")
	}
}

// LoadGuestImagePolicy reads the fixed catalog beneath a protected packaged
// release root, verifies its signature and architecture, and returns immutable
// policy. The catalog path is not caller-selectable.
func LoadGuestImagePolicy(releaseRoot, architecture string, trust GuestImageTrust) (*GuestImagePolicy, error) {
	if len(trust.keys) == 0 {
		return nil, serviceError(CodeUnavailable, "guest image trust is unavailable")
	}
	root, err := normalizeServiceRoot("release root", releaseRoot)
	if err != nil {
		return nil, err
	}
	catalogPath := filepath.Join(root, filepath.FromSlash(GuestImageCatalogRelativePath))
	if err := rejectSymlinkedPath(root, filepath.FromSlash(GuestImageCatalogRelativePath)); err != nil {
		return nil, serviceError(CodePermissionDenied, "guest image catalog path is not service-owned")
	}
	data, err := readBoundedRegularFile(catalogPath, maxGuestImageCatalogBytes)
	if err != nil {
		return nil, serviceError(CodeUnavailable, "read guest image catalog: %v", err)
	}
	return VerifyGuestImageCatalog(data, architecture, trust)
}

// VerifyGuestImageCatalog verifies strict structure, architecture, canonical
// inventory ordering, and the Ed25519 signature from a compiled trust anchor.
func VerifyGuestImageCatalog(data []byte, architecture string, trust GuestImageTrust) (*GuestImagePolicy, error) {
	if architecture != "x64" && architecture != "arm64" {
		return nil, serviceError(CodeInvalidArgument, "guest image architecture is unsupported")
	}
	if len(data) == 0 || len(data) > maxGuestImageCatalogBytes {
		return nil, serviceError(CodePermissionDenied, "guest image catalog is missing or oversized")
	}
	if err := rejectDuplicateJSONKeys(data); err != nil {
		return nil, serviceError(CodePermissionDenied, "guest image catalog JSON is malformed")
	}
	var catalog GuestImageCatalog
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&catalog); err != nil {
		return nil, serviceError(CodePermissionDenied, "guest image catalog structure is invalid")
	}
	if err := ensureJSONEOF(decoder); err != nil {
		return nil, serviceError(CodePermissionDenied, "guest image catalog must contain one object")
	}
	if catalog.SchemaVersion != guestImageCatalogVersion || catalog.Product != "grok-desktop-guest" ||
		catalog.Architecture != architecture || catalog.Sequence == 0 || catalog.Sequence > maxGuestCatalogSequence || len(catalog.Images) == 0 ||
		len(catalog.Images) > maxOfficialGuestImages {
		return nil, serviceError(CodePermissionDenied, "guest image catalog header is invalid")
	}
	if catalog.Signature.Algorithm != "ed25519" || !guestCatalogKeyIDPattern.MatchString(catalog.Signature.KeyID) {
		return nil, serviceError(CodePermissionDenied, "guest image catalog signature metadata is invalid")
	}
	publicKey, trusted := trust.keys[catalog.Signature.KeyID]
	if !trusted {
		return nil, serviceError(CodePermissionDenied, "guest image catalog signing key is not trusted")
	}
	signature, err := decodeCanonicalBase64(catalog.Signature.Value, ed25519.SignatureSize)
	if err != nil {
		return nil, serviceError(CodePermissionDenied, "guest image catalog signature is invalid")
	}
	signingBytes, err := GuestImageCatalogSigningBytes(catalog)
	if err != nil || !ed25519.Verify(publicKey, signingBytes, signature) {
		return nil, serviceError(CodePermissionDenied, "guest image catalog signature is invalid")
	}

	images := make(map[string]OfficialGuestImage, len(catalog.Images))
	stagingNames := make(map[string]struct{}, len(catalog.Images))
	previousID := ""
	for _, image := range catalog.Images {
		if validateID("guest image id", image.ID) != nil || !catalogPathSegmentIsSafe(image.ID) || !validGuestImageVersion(image.Version) ||
			!guestStagingNamePattern.MatchString(image.StagingName) || filepath.Base(image.StagingName) != image.StagingName ||
			!catalogPathSegmentIsSafe(image.StagingName) ||
			!sha256Pattern.MatchString(image.SHA256) || image.SHA256 != strings.ToLower(image.SHA256) ||
			image.SizeBytes <= 0 || image.SizeBytes > maxImageSizeByte || image.ID <= previousID {
			return nil, serviceError(CodePermissionDenied, "guest image catalog inventory is invalid")
		}
		if _, duplicate := images[image.ID]; duplicate {
			return nil, serviceError(CodePermissionDenied, "guest image catalog contains a duplicate image")
		}
		if _, duplicate := stagingNames[image.StagingName]; duplicate {
			return nil, serviceError(CodePermissionDenied, "guest image catalog contains a duplicate staging name")
		}
		images[image.ID] = image
		stagingNames[image.StagingName] = struct{}{}
		previousID = image.ID
	}
	digest := sha256.Sum256(data)
	return &GuestImagePolicy{
		architecture: architecture, sequence: catalog.Sequence, catalogSHA256: hex.EncodeToString(digest[:]),
		signingKeyID: catalog.Signature.KeyID, images: images,
	}, nil
}

func validGuestImageVersion(value string) bool {
	if len(value) == 0 || len(value) > 128 || !guestImageVersionPattern.MatchString(value) {
		return false
	}
	coreAndBuild := strings.SplitN(value, "+", 2)
	if len(coreAndBuild) == 2 {
		for _, identifier := range strings.Split(coreAndBuild[1], ".") {
			if identifier == "" {
				return false
			}
		}
	}
	coreAndPrerelease := strings.SplitN(coreAndBuild[0], "-", 2)
	if len(coreAndPrerelease) == 1 {
		return true
	}
	for _, identifier := range strings.Split(coreAndPrerelease[1], ".") {
		if identifier == "" {
			return false
		}
		numeric := true
		for _, character := range identifier {
			if character < '0' || character > '9' {
				numeric = false
				break
			}
		}
		if numeric && len(identifier) > 1 && identifier[0] == '0' {
			return false
		}
	}
	return true
}

func catalogPathSegmentIsSafe(value string) bool {
	base := strings.ToUpper(strings.SplitN(value, ".", 2)[0])
	_, reserved := windowsReservedNames[base]
	return !reserved
}

// GuestImageCatalogSigningBytes is the sole canonicalization contract used by
// trusted release tooling and the service verifier.
func GuestImageCatalogSigningBytes(catalog GuestImageCatalog) ([]byte, error) {
	document := guestImageCatalogSigningDocument{
		SchemaVersion: catalog.SchemaVersion,
		Product:       catalog.Product,
		Architecture:  catalog.Architecture,
		Sequence:      catalog.Sequence,
		Images:        append([]OfficialGuestImage(nil), catalog.Images...),
		Signature: guestCatalogSignatureIdentity{
			Algorithm: catalog.Signature.Algorithm,
			KeyID:     catalog.Signature.KeyID,
		},
	}
	encoded, err := json.Marshal(document)
	if err != nil {
		return nil, err
	}
	return append(encoded, '\n'), nil
}

// EnforceGuestImagePolicyRollback records the greatest accepted catalog
// sequence before the service begins accepting requests. Reuse of a sequence
// with different signed bytes is rejected as equivocation.
func EnforceGuestImagePolicyRollback(dataRoot string, policy *GuestImagePolicy) error {
	if policy == nil || policy.sequence == 0 || policy.catalogSHA256 == "" {
		return serviceError(CodeUnavailable, "verified guest image policy is unavailable")
	}
	root, err := normalizeServiceRoot("service data root", dataRoot)
	if err != nil {
		return err
	}
	if err := os.MkdirAll(root, 0o700); err != nil {
		return serviceError(CodeUnavailable, "create guest image policy state root: %v", err)
	}
	return withGuestImagePolicyLock(root, func() error {
		return enforceGuestImagePolicyRollbackLocked(root, policy)
	})
}

func enforceGuestImagePolicyRollbackLocked(root string, policy *GuestImagePolicy) error {
	statePath := filepath.Join(root, "guest-image-policy.json")
	if err := rejectRedirectingPathIfPresent(root, "guest-image-policy.json"); err != nil {
		return serviceError(CodeUnavailable, "guest image policy state path is unsafe")
	}
	data, readErr := readBoundedRegularFile(statePath, maxGuestImagePolicyStateBytes)
	if readErr == nil {
		var state guestImagePolicyState
		if err := rejectDuplicateJSONKeys(data); err != nil {
			return serviceError(CodeUnavailable, "guest image policy state is malformed")
		}
		decoder := json.NewDecoder(bytes.NewReader(data))
		decoder.DisallowUnknownFields()
		if err := decoder.Decode(&state); err != nil || ensureJSONEOF(decoder) != nil ||
			state.Version != guestImagePolicyStateVersion || state.Sequence == 0 ||
			state.Architecture == "" || !sha256Pattern.MatchString(state.CatalogSHA256) ||
			!guestCatalogKeyIDPattern.MatchString(state.SigningKeyID) {
			return serviceError(CodeUnavailable, "guest image policy state is invalid")
		}
		if state.Architecture != policy.architecture || state.Sequence > policy.sequence {
			return serviceError(CodePermissionDenied, "guest image catalog rollback was rejected")
		}
		if state.Sequence == policy.sequence {
			if state.CatalogSHA256 != policy.catalogSHA256 {
				return serviceError(CodePermissionDenied, "guest image catalog sequence was reused with different content")
			}
			return nil
		}
	} else if !errors.Is(readErr, os.ErrNotExist) {
		return serviceError(CodeUnavailable, "read guest image policy state: %v", readErr)
	}
	return persistGuestImagePolicyState(root, guestImagePolicyState{
		Version: guestImagePolicyStateVersion, Architecture: policy.architecture, Sequence: policy.sequence,
		CatalogSHA256: policy.catalogSHA256, SigningKeyID: policy.signingKeyID,
	})
}

func (policy *GuestImagePolicy) clone() *GuestImagePolicy {
	if policy == nil {
		return nil
	}
	images := make(map[string]OfficialGuestImage, len(policy.images))
	for id, image := range policy.images {
		images[id] = image
	}
	return &GuestImagePolicy{
		architecture: policy.architecture, sequence: policy.sequence, catalogSHA256: policy.catalogSHA256,
		signingKeyID: policy.signingKeyID, images: images,
	}
}

func (policy *GuestImagePolicy) image(id string) (OfficialGuestImage, bool) {
	if policy == nil {
		return OfficialGuestImage{}, false
	}
	image, ok := policy.images[id]
	return image, ok
}

func persistGuestImagePolicyState(root string, state guestImagePolicyState) error {
	temporary, err := os.CreateTemp(root, ".guest-image-policy-*.json")
	if err != nil {
		return serviceError(CodeUnavailable, "create guest image policy transaction: %v", err)
	}
	temporaryPath := temporary.Name()
	committed := false
	defer func() {
		_ = temporary.Close()
		if !committed {
			_ = os.Remove(temporaryPath)
		}
	}()
	if err := temporary.Chmod(0o600); err != nil {
		return serviceError(CodeUnavailable, "secure guest image policy transaction: %v", err)
	}
	if err := json.NewEncoder(temporary).Encode(state); err != nil {
		return serviceError(CodeUnavailable, "encode guest image policy state: %v", err)
	}
	if err := temporary.Sync(); err != nil {
		return serviceError(CodeUnavailable, "flush guest image policy state: %v", err)
	}
	if err := temporary.Close(); err != nil {
		return serviceError(CodeUnavailable, "close guest image policy state: %v", err)
	}
	if err := atomicReplace(temporaryPath, filepath.Join(root, "guest-image-policy.json")); err != nil {
		return serviceError(CodeUnavailable, "commit guest image policy state: %v", err)
	}
	committed = true
	return nil
}

func normalizeServiceRoot(name, root string) (string, error) {
	if root == "" || !filepath.IsAbs(root) {
		return "", serviceError(CodeInvalidArgument, "%s must be an absolute path", name)
	}
	clean := filepath.Clean(root)
	volumeRoot := filepath.VolumeName(clean) + string(filepath.Separator)
	if clean == string(filepath.Separator) || strings.EqualFold(clean, volumeRoot) {
		return "", serviceError(CodeInvalidArgument, "%s cannot be a filesystem root", name)
	}
	return clean, nil
}

func rejectSymlinkedPath(root, relative string) error {
	current := filepath.Clean(root)
	components := append([]string{current}, strings.Split(filepath.Clean(relative), string(filepath.Separator))...)
	for index, component := range components {
		if index > 0 {
			current = filepath.Join(current, component)
		}
		information, err := os.Lstat(current)
		if err != nil {
			return err
		}
		redirecting, err := pathComponentIsRedirecting(current, information)
		if err != nil {
			return err
		}
		if redirecting {
			return fmt.Errorf("path contains a redirecting filesystem object")
		}
	}
	return nil
}

func rejectRedirectingPathIfPresent(root, relative string) error {
	err := rejectSymlinkedPath(root, relative)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	return err
}

func readBoundedRegularFile(path string, maximum int64) ([]byte, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	before, err := file.Stat()
	if err != nil {
		return nil, err
	}
	if !before.Mode().IsRegular() || before.Size() < 1 || before.Size() > maximum {
		return nil, fmt.Errorf("file is not a bounded regular file")
	}
	data, err := io.ReadAll(io.LimitReader(file, maximum+1))
	if err != nil || int64(len(data)) != before.Size() {
		return nil, fmt.Errorf("file changed while being read")
	}
	after, err := file.Stat()
	if err != nil || !os.SameFile(before, after) || after.Size() != before.Size() {
		return nil, fmt.Errorf("file identity changed while being read")
	}
	return data, nil
}

func decodeCanonicalBase64(value string, expected int) ([]byte, error) {
	decoded, err := base64.StdEncoding.Strict().DecodeString(value)
	if err != nil || len(decoded) != expected || base64.StdEncoding.EncodeToString(decoded) != value {
		return nil, fmt.Errorf("invalid base64")
	}
	return decoded, nil
}

func rejectDuplicateJSONKeys(data []byte) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := consumeStrictJSONValue(decoder, 0); err != nil {
		return err
	}
	if _, err := decoder.Token(); !errors.Is(err, io.EOF) {
		return fmt.Errorf("JSON contains multiple values")
	}
	return nil
}

func consumeStrictJSONValue(decoder *json.Decoder, depth int) error {
	if depth > 64 {
		return fmt.Errorf("JSON nesting exceeds its limit")
	}
	token, err := decoder.Token()
	if err != nil {
		return err
	}
	delimiter, structured := token.(json.Delim)
	if !structured {
		return nil
	}
	switch delimiter {
	case '{':
		keys := make(map[string]struct{})
		for decoder.More() {
			keyToken, err := decoder.Token()
			if err != nil {
				return err
			}
			key, ok := keyToken.(string)
			if !ok {
				return fmt.Errorf("JSON object key is not a string")
			}
			if _, duplicate := keys[key]; duplicate {
				return fmt.Errorf("JSON object contains a duplicate key")
			}
			keys[key] = struct{}{}
			if err := consumeStrictJSONValue(decoder, depth+1); err != nil {
				return err
			}
		}
		closing, err := decoder.Token()
		if err != nil || closing != json.Delim('}') {
			return fmt.Errorf("JSON object is not terminated")
		}
	case '[':
		for decoder.More() {
			if err := consumeStrictJSONValue(decoder, depth+1); err != nil {
				return err
			}
		}
		closing, err := decoder.Token()
		if err != nil || closing != json.Delim(']') {
			return fmt.Errorf("JSON array is not terminated")
		}
	default:
		return fmt.Errorf("unexpected JSON delimiter")
	}
	return nil
}
