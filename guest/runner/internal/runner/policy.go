package runner

import (
	"crypto/ed25519"
	"encoding/base64"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/grok-insider/grok-desktop/guest/runner/internal/strictjson"
)

var publisherIDPattern = regexp.MustCompile(`^[a-z][a-z0-9.-]{1,127}$`)

const (
	protocolVersion = "1.0.0"
	defaultPort     = 4050
)

type Policy struct {
	Version                       int               `json:"version"`
	ImageVersion                  string            `json:"imageVersion"`
	ManifestRoots                 []string          `json:"manifestRoots"`
	TrustedManifestKeyFiles       []string          `json:"trustedManifestKeyFiles"`
	PublisherTrust                map[string]string `json:"publisherTrust"`
	UnsignedDevelopmentPublishers []string          `json:"unsignedDevelopmentPublishers"`
	WorkspaceRoot                 string            `json:"workspaceRoot"`
	StateRoot                     string            `json:"stateRoot"`
	AllowUnsignedDevelopment      bool              `json:"allowUnsignedDevelopment"`
	MaxMessageBytes               int               `json:"maxMessageBytes"`
	ControlPort                   uint32            `json:"controlPort"`
	BundleOwnerUID                uint32            `json:"bundleOwnerUid"`
	BubblewrapPath                string            `json:"bubblewrapPath"`
	ComputerUseSchema             string            `json:"computerUseSchema"`
	WorkspaceMounterSocket        string            `json:"workspaceMounterSocket"`
	Transport                     TransportPolicy   `json:"transport"`
}

type TransportPolicy struct {
	Family  string `json:"family"`
	Purpose string `json:"purpose"`
}

type trustedKeyFile struct {
	Version     int    `json:"version"`
	PublisherID string `json:"publisherId"`
	Trust       string `json:"trust"`
	KeyID       string `json:"keyId"`
	PublicKey   string `json:"publicKey"`
}

type TrustStore struct {
	Keys           map[string]map[string]ed25519.PublicKey
	PublisherTrust map[string]string
}

func LoadPolicy(path string) (Policy, TrustStore, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return Policy{}, TrustStore{}, fmt.Errorf("read policy: %w", err)
	}
	var policy Policy
	if err := strictjson.Decode(data, 256<<10, &policy); err != nil {
		return Policy{}, TrustStore{}, fmt.Errorf("invalid policy: %w", err)
	}
	if err := policy.Validate(); err != nil {
		return Policy{}, TrustStore{}, err
	}
	store := TrustStore{
		Keys:           make(map[string]map[string]ed25519.PublicKey),
		PublisherTrust: make(map[string]string, len(policy.PublisherTrust)),
	}
	for publisher, trust := range policy.PublisherTrust {
		store.PublisherTrust[publisher] = trust
	}
	for _, keyPath := range policy.TrustedManifestKeyFiles {
		keyData, err := os.ReadFile(keyPath)
		if err != nil {
			return Policy{}, TrustStore{}, fmt.Errorf("read trusted key: %w", err)
		}
		var record trustedKeyFile
		if err := strictjson.Decode(keyData, 16<<10, &record); err != nil {
			return Policy{}, TrustStore{}, fmt.Errorf("invalid trusted key file: %w", err)
		}
		decoded, err := base64.StdEncoding.Strict().DecodeString(record.PublicKey)
		if record.Version != 1 || record.PublisherID == "" || record.KeyID == "" ||
			(record.Trust != "first-party" && record.Trust != "third-party") ||
			err != nil || len(decoded) != ed25519.PublicKeySize {
			return Policy{}, TrustStore{}, errors.New("invalid trusted key record")
		}
		if expected, exists := store.PublisherTrust[record.PublisherID]; exists && expected != record.Trust {
			return Policy{}, TrustStore{}, errors.New("conflicting publisher trust records")
		}
		store.PublisherTrust[record.PublisherID] = record.Trust
		publisherKeys := store.Keys[record.PublisherID]
		if publisherKeys == nil {
			publisherKeys = make(map[string]ed25519.PublicKey)
			store.Keys[record.PublisherID] = publisherKeys
		}
		if _, duplicate := publisherKeys[record.KeyID]; duplicate {
			return Policy{}, TrustStore{}, errors.New("duplicate trusted key record")
		}
		publisherKeys[record.KeyID] = ed25519.PublicKey(decoded)
	}
	return policy, store, nil
}

func (policy *Policy) Validate() error {
	if policy.Version != 1 || policy.ImageVersion == "" || len(policy.ImageVersion) > 128 {
		return errors.New("policy version or image identity is invalid")
	}
	if len(policy.ManifestRoots) == 0 || len(policy.ManifestRoots) > 8 {
		return errors.New("policy must contain bounded manifest roots")
	}
	seenPaths := make(map[string]struct{})
	for _, root := range policy.ManifestRoots {
		if !filepath.IsAbs(root) || filepath.Clean(root) != root {
			return errors.New("manifest root must be an absolute canonical path")
		}
		if _, duplicate := seenPaths[root]; duplicate {
			return errors.New("manifest root is duplicated")
		}
		seenPaths[root] = struct{}{}
	}
	if len(policy.TrustedManifestKeyFiles) > 32 {
		return errors.New("trusted manifest key set exceeds its limit")
	}
	for _, path := range policy.TrustedManifestKeyFiles {
		if !filepath.IsAbs(path) || filepath.Clean(path) != path {
			return errors.New("trusted manifest key path must be absolute and canonical")
		}
		if _, duplicate := seenPaths[path]; duplicate {
			return errors.New("trusted policy path is duplicated")
		}
		seenPaths[path] = struct{}{}
	}
	for _, path := range []string{
		policy.WorkspaceRoot, policy.StateRoot, policy.BubblewrapPath,
		policy.ComputerUseSchema, policy.WorkspaceMounterSocket,
	} {
		if !filepath.IsAbs(path) || filepath.Clean(path) != path {
			return errors.New("policy paths must be absolute and canonical")
		}
	}
	if policy.WorkspaceRoot == policy.StateRoot {
		return errors.New("workspace and state roots must be distinct")
	}
	if pathWithin(policy.WorkspaceRoot, policy.WorkspaceMounterSocket) || pathWithin(policy.StateRoot, policy.WorkspaceMounterSocket) {
		return errors.New("workspace mounter socket must be outside managed data roots")
	}
	if policy.BundleOwnerUID != 0 {
		return errors.New("trusted bundles and workspace mounts must be root-owned")
	}
	if policy.MaxMessageBytes < 4096 || policy.MaxMessageBytes > 16<<20 {
		return errors.New("policy message limit is outside its bounds")
	}
	if policy.ControlPort == 0 {
		policy.ControlPort = defaultPort
	}
	if policy.ControlPort != defaultPort || policy.Transport.Family != "AF_VSOCK" || policy.Transport.Purpose != "control" {
		return errors.New("policy transport does not match the allowlisted control socket")
	}
	if len(policy.PublisherTrust) > 64 || len(policy.UnsignedDevelopmentPublishers) > 16 {
		return errors.New("publisher policy exceeds its limit")
	}
	for publisher, trust := range policy.PublisherTrust {
		if !publisherIDPattern.MatchString(publisher) || (trust != "first-party" && trust != "third-party") {
			return errors.New("publisher trust policy is invalid")
		}
	}
	if policy.AllowUnsignedDevelopment && len(policy.UnsignedDevelopmentPublishers) == 0 {
		return errors.New("unsigned development policy must be publisher-scoped")
	}
	unsigned := make(map[string]struct{}, len(policy.UnsignedDevelopmentPublishers))
	for _, publisher := range policy.UnsignedDevelopmentPublishers {
		if !publisherIDPattern.MatchString(publisher) || policy.PublisherTrust[publisher] == "" {
			return errors.New("unsigned development publisher is not trusted")
		}
		if _, duplicate := unsigned[publisher]; duplicate {
			return errors.New("unsigned development publisher is duplicated")
		}
		unsigned[publisher] = struct{}{}
	}
	return nil
}

func pathWithin(root, candidate string) bool {
	relative, err := filepath.Rel(root, candidate)
	return err == nil && (relative == "." || (relative != ".." && !strings.HasPrefix(relative, ".."+string(filepath.Separator))))
}

func (policy Policy) unsignedPublishers() map[string]struct{} {
	result := make(map[string]struct{}, len(policy.UnsignedDevelopmentPublishers))
	for _, publisher := range policy.UnsignedDevelopmentPublishers {
		result[publisher] = struct{}{}
	}
	return result
}
