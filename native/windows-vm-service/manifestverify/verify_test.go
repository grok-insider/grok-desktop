package manifestverify

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"testing"
)

func TestVerifySignedManifest(t *testing.T) {
	manifest, privateKey, policy := validManifest(t)
	encoded := signManifest(t, manifest, privateKey)
	verified, err := Verify(encoded, policy)
	if err != nil {
		t.Fatalf("Verify: %v", err)
	}
	if verified.ID != manifest.ID {
		t.Fatalf("verified ID = %q", verified.ID)
	}
}

func TestVerifyRejectsUnsignedStableManifest(t *testing.T) {
	manifest, _, policy := validManifest(t)
	manifest.Signature = Signature{Algorithm: "none"}
	encoded := marshalManifest(t, manifest)
	assertVerificationCode(t, verifyResult(encoded, policy), CodeUnsignedRelease)
}

func TestVerifyRejectsPathTraversal(t *testing.T) {
	manifest, _, policy := validManifest(t)
	for name, mutate := range map[string]func(*Manifest){
		"command": func(manifest *Manifest) { manifest.Entrypoint.Command = "../bin/adapter" },
		"adapter": func(manifest *Manifest) { manifest.Entrypoint.Adapter = `..\\adapter.json` },
		"config":  func(manifest *Manifest) { manifest.ConfigSchema = "/etc/passwd" },
	} {
		t.Run(name, func(t *testing.T) {
			candidate := manifest
			mutate(&candidate)
			assertVerificationCode(t, verifyResult(marshalManifest(t, candidate), policy), CodeUnsafePath)
		})
	}
}

func TestVerifyRejectsCapabilityEscalation(t *testing.T) {
	manifest, privateKey, policy := validManifest(t)
	manifest.Capabilities = append(manifest.Capabilities, "host.process.exec")
	assertVerificationCode(t, verifyResult(signManifest(t, manifest, privateKey), policy), CodeCapabilityEscalation)
}

func TestVerifyRejectsIncompatibleProtocol(t *testing.T) {
	manifest, privateKey, policy := validManifest(t)
	manifest.Protocol = ProtocolRange{MinInclusive: "2.0.0", MaxExclusive: "3.0.0"}
	assertVerificationCode(t, verifyResult(signManifest(t, manifest, privateKey), policy), CodeIncompatibleProtocol)
}

func TestVerifyRejectsSignatureTampering(t *testing.T) {
	manifest, privateKey, policy := validManifest(t)
	encoded := signManifest(t, manifest, privateKey)
	var tampered map[string]any
	if err := json.Unmarshal(encoded, &tampered); err != nil {
		t.Fatalf("decode signed manifest: %v", err)
	}
	tampered["version"] = "9.9.9"
	encoded, _ = json.Marshal(tampered)
	assertVerificationCode(t, verifyResult(encoded, policy), CodeInvalidSignature)
}

func TestVerifyAllowsExplicitUnsignedDevelopmentPolicy(t *testing.T) {
	manifest, _, policy := validManifest(t)
	manifest.UpdateChannel = "development"
	manifest.Signature = Signature{Algorithm: "none"}
	policy.AllowUnsignedDevelopment = true
	if _, err := Verify(marshalManifest(t, manifest), policy); err != nil {
		t.Fatalf("Verify unsigned development manifest: %v", err)
	}
}

func TestResolveBundlePathRejectsSymlinkEscape(t *testing.T) {
	root := t.TempDir()
	outside := t.TempDir()
	if err := os.WriteFile(filepath.Join(outside, "adapter"), []byte("test"), 0o600); err != nil {
		t.Fatalf("write outside file: %v", err)
	}
	if err := os.Symlink(outside, filepath.Join(root, "bin")); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}
	_, err := ResolveBundlePath(root, "bin/adapter")
	assertVerificationCode(t, err, CodeUnsafePath)
}

func validManifest(t *testing.T) (Manifest, ed25519.PrivateKey, Policy) {
	t.Helper()
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("GenerateKey: %v", err)
	}
	keyID := "release-key-1"
	emptySignature := ""
	manifest := Manifest{
		ManifestVersion: 1,
		ID:              "desktop.grok.wisp",
		Version:         "1.0.0",
		Protocol:        ProtocolRange{MinInclusive: "1.0.0", MaxExclusive: "2.0.0"},
		Entrypoint:      Entrypoint{Command: "bin/adapter", Arguments: []string{"--stdio"}, Adapter: "adapter.json"},
		Publisher:       Publisher{ID: "grok-insider", Name: "Grok Desktop", Trust: "first-party"},
		Signature:       Signature{Algorithm: "ed25519", KeyID: &keyID, Value: &emptySignature},
		Capabilities:    []string{"computer-use.observe"},
		ConfigSchema:    "config.schema.json",
		Permissions: Permissions{
			Filesystem: FilesystemPermissions{ReadOnlyRoots: []string{}, ReadWriteRoots: []string{}},
			Network:    NetworkPermissions{Outbound: []NetworkEndpoint{}, Listen: []ListenEndpoint{}},
			Process:    ProcessPermissions{Spawn: []string{}},
			Devices:    []string{}, Secrets: []string{}, HostCapabilities: []string{},
		},
		UpdateChannel: "stable",
		Lifecycle: Lifecycle{
			Scope: "integration", RestartPolicy: "on-failure", ShutdownTimeoutMS: 5000,
			HealthCheck: HealthCheck{Method: "lifecycle.health", IntervalMS: 10000, TimeoutMS: 2000, FailureThreshold: 3},
		},
	}
	policy := Policy{
		SupportedProtocol: "1.0.0",
		TrustedKeys: map[string]map[string]ed25519.PublicKey{
			"grok-insider": {keyID: publicKey},
		},
		AllowedCapabilities:           map[string]struct{}{"computer-use.observe": {}},
		PublisherTrust:                map[string]string{"grok-insider": "first-party"},
		UnsignedDevelopmentPublishers: map[string]struct{}{"grok-insider": {}},
	}
	return manifest, privateKey, policy
}

func signManifest(t *testing.T, manifest Manifest, privateKey ed25519.PrivateKey) []byte {
	t.Helper()
	canonical, err := SigningBytes(manifest)
	if err != nil {
		t.Fatalf("SigningBytes: %v", err)
	}
	signature := base64.StdEncoding.EncodeToString(ed25519.Sign(privateKey, canonical))
	manifest.Signature.Value = &signature
	return marshalManifest(t, manifest)
}

func marshalManifest(t *testing.T, manifest Manifest) []byte {
	t.Helper()
	encoded, err := json.Marshal(manifest)
	if err != nil {
		t.Fatalf("marshal manifest: %v", err)
	}
	return encoded
}

func verifyResult(data []byte, policy Policy) error {
	_, err := Verify(data, policy)
	return err
}

func assertVerificationCode(t *testing.T, err error, code ErrorCode) {
	t.Helper()
	if err == nil {
		t.Fatalf("expected %q error", code)
	}
	var verificationErr *Error
	if !errors.As(err, &verificationErr) {
		t.Fatalf("error = %T %v, want *Error", err, err)
	}
	if verificationErr.Code != code {
		t.Fatalf("error code = %q, want %q: %v", verificationErr.Code, code, err)
	}
}
