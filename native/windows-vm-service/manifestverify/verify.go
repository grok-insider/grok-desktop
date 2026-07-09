package manifestverify

import (
	"bytes"
	"crypto/ed25519"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"path"
	"path/filepath"
	"regexp"
	"strings"
	"unicode"
	"unicode/utf8"
)

const MaxManifestBytes = 64 << 10

type ErrorCode string

const (
	CodeInvalidManifest      ErrorCode = "invalid_manifest"
	CodeUnsignedRelease      ErrorCode = "unsigned_release"
	CodeUntrustedSignature   ErrorCode = "untrusted_signature"
	CodeInvalidSignature     ErrorCode = "invalid_signature"
	CodeUnsafePath           ErrorCode = "unsafe_path"
	CodeCapabilityEscalation ErrorCode = "capability_escalation"
	CodeIncompatibleProtocol ErrorCode = "incompatible_protocol"
)

type Error struct {
	Code    ErrorCode
	Message string
}

func (e *Error) Error() string {
	return fmt.Sprintf("manifest verification: %s: %s", e.Code, e.Message)
}

type Policy struct {
	SupportedProtocol             string
	TrustedKeys                   map[string]map[string]ed25519.PublicKey
	AllowedCapabilities           map[string]struct{}
	PublisherTrust                map[string]string
	UnsignedDevelopmentPublishers map[string]struct{}
	AllowUnsignedDevelopment      bool
}

var (
	drivePathPattern = regexp.MustCompile(`^[A-Za-z]:`)
)

var windowsReservedNames = map[string]struct{}{
	"CON": {}, "PRN": {}, "AUX": {}, "NUL": {},
	"COM1": {}, "COM2": {}, "COM3": {}, "COM4": {}, "COM5": {},
	"COM6": {}, "COM7": {}, "COM8": {}, "COM9": {},
	"LPT1": {}, "LPT2": {}, "LPT3": {}, "LPT4": {}, "LPT5": {},
	"LPT6": {}, "LPT7": {}, "LPT8": {}, "LPT9": {},
}

func Verify(data []byte, policy Policy) (*Manifest, error) {
	manifest, err := Decode(data)
	if err != nil {
		return nil, err
	}
	if err := validateManifest(manifest, policy); err != nil {
		return nil, err
	}
	if err := verifySignature(manifest, policy); err != nil {
		return nil, err
	}
	return manifest, nil
}

func Decode(data []byte) (*Manifest, error) {
	if len(data) == 0 || len(data) > MaxManifestBytes {
		return nil, verificationError(CodeInvalidManifest, "manifest size must be between 1 and %d bytes", MaxManifestBytes)
	}
	if !utf8.Valid(data) {
		return nil, verificationError(CodeInvalidManifest, "manifest must be valid UTF-8 JSON")
	}
	if err := rejectDuplicateJSONKeys(data); err != nil {
		return nil, err
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	var manifest Manifest
	if err := decoder.Decode(&manifest); err != nil {
		return nil, verificationError(CodeInvalidManifest, "manifest JSON does not match the typed schema")
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		return nil, verificationError(CodeInvalidManifest, "manifest must contain one JSON object")
	}
	if err := validateRequiredPresence(data); err != nil {
		return nil, err
	}
	return &manifest, nil
}

func SigningBytes(manifest Manifest) ([]byte, error) {
	empty := ""
	manifest.Signature.Value = &empty
	encoded, err := json.Marshal(manifest)
	if err != nil {
		return nil, fmt.Errorf("canonicalize manifest: %w", err)
	}
	return encoded, nil
}

func validateProtocol(protocol ProtocolRange, supportedValue string) error {
	if utf8.RuneCountInString(protocol.MinInclusive) > 64 || utf8.RuneCountInString(protocol.MaxExclusive) > 64 {
		return verificationError(CodeInvalidManifest, "protocol semantic version exceeds its length limit")
	}
	minimum, err := parseSemanticVersion(protocol.MinInclusive)
	if err != nil {
		return verificationError(CodeInvalidManifest, "minimum protocol semantic version is invalid")
	}
	maximum, err := parseSemanticVersion(protocol.MaxExclusive)
	if err != nil {
		return verificationError(CodeInvalidManifest, "maximum protocol semantic version is invalid")
	}
	if compareSemanticVersion(minimum, maximum) >= 0 {
		return verificationError(CodeInvalidManifest, "protocol range is empty or inverted")
	}
	supported, err := parseSemanticVersion(supportedValue)
	if err != nil {
		return verificationError(CodeInvalidManifest, "verification policy has an invalid supported protocol")
	}
	if compareSemanticVersion(supported, minimum) < 0 || compareSemanticVersion(supported, maximum) >= 0 {
		return verificationError(CodeIncompatibleProtocol, "supported protocol is outside the manifest range")
	}
	return nil
}

func verifySignature(manifest *Manifest, policy Policy) error {
	if manifest.Signature.Algorithm == "none" {
		if manifest.UpdateChannel != "development" {
			return verificationError(CodeUnsignedRelease, "non-development manifests must be signed")
		}
		if !policy.AllowUnsignedDevelopment {
			return verificationError(CodeUnsignedRelease, "unsigned development manifests are disabled")
		}
		if _, allowed := policy.UnsignedDevelopmentPublishers[manifest.Publisher.ID]; !allowed {
			return verificationError(CodeUnsignedRelease, "publisher is not allowed to use unsigned development manifests")
		}
		if manifest.Signature.KeyID != nil || manifest.Signature.Value != nil {
			return verificationError(CodeInvalidManifest, "unsigned signature metadata must use null keyId and value")
		}
		return nil
	}
	if manifest.Signature.Algorithm != "ed25519" || manifest.Signature.KeyID == nil || manifest.Signature.Value == nil {
		return verificationError(CodeInvalidSignature, "signature must use ed25519 with keyId and value")
	}
	publisherKeys := policy.TrustedKeys[manifest.Publisher.ID]
	publicKey, trusted := publisherKeys[*manifest.Signature.KeyID]
	if !trusted || len(publicKey) != ed25519.PublicKeySize {
		return verificationError(CodeUntrustedSignature, "publisher key is not trusted")
	}
	signature, err := base64.StdEncoding.Strict().DecodeString(*manifest.Signature.Value)
	if err != nil || len(signature) != ed25519.SignatureSize {
		return verificationError(CodeInvalidSignature, "signature value is not a canonical Ed25519 signature")
	}
	canonical, err := SigningBytes(*manifest)
	if err != nil {
		return verificationError(CodeInvalidManifest, "manifest canonicalization failed")
	}
	if !ed25519.Verify(publicKey, canonical, signature) {
		return verificationError(CodeInvalidSignature, "Ed25519 verification failed")
	}
	return nil
}

func ValidateBundlePath(value string) error {
	if value == "" || len(value) > 260 || !bundlePathPattern.MatchString(value) || strings.Contains(value, `\`) || strings.HasPrefix(value, "/") || drivePathPattern.MatchString(value) {
		return fmt.Errorf("path must be a non-empty bundle-relative slash path")
	}
	segments := strings.Split(value, "/")
	for _, segment := range segments {
		if segment == "" || segment == "." || segment == ".." || strings.Contains(segment, ":") || strings.IndexFunc(segment, unicode.IsControl) >= 0 {
			return fmt.Errorf("path is not canonical or contains traversal")
		}
		if strings.TrimRight(segment, " .") != segment {
			return fmt.Errorf("path segments cannot end in a dot or space")
		}
		base := strings.ToUpper(strings.SplitN(segment, ".", 2)[0])
		if _, reserved := windowsReservedNames[base]; reserved {
			return fmt.Errorf("path contains a reserved Windows name")
		}
	}
	if path.Clean(value) != value {
		return fmt.Errorf("path is not canonical")
	}
	return nil
}

func ResolveBundlePath(root, relative string) (string, error) {
	if err := ValidateBundlePath(relative); err != nil {
		return "", verificationError(CodeUnsafePath, "%v", err)
	}
	if !filepath.IsAbs(root) {
		return "", verificationError(CodeUnsafePath, "bundle root must be absolute")
	}
	resolvedRoot, err := filepath.EvalSymlinks(filepath.Clean(root))
	if err != nil {
		return "", verificationError(CodeUnsafePath, "bundle root cannot be resolved")
	}
	candidate, err := filepath.EvalSymlinks(filepath.Join(resolvedRoot, filepath.FromSlash(relative)))
	if err != nil {
		return "", verificationError(CodeUnsafePath, "bundle path cannot be resolved")
	}
	rel, err := filepath.Rel(resolvedRoot, candidate)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return "", verificationError(CodeUnsafePath, "resolved path escapes bundle root")
	}
	return candidate, nil
}

func verificationError(code ErrorCode, format string, args ...any) error {
	return &Error{Code: code, Message: fmt.Sprintf(format, args...)}
}
