package manifestverify

import (
	"crypto/ed25519"
	"encoding/base64"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

// TestWispFixtureBundle verifies the committed Ed25519-signed Wisp test
// bundle under integrations/testdata/wisp-signed. This is the signed proof
// artifact for host lifecycle tests (not the development algorithm:none source).
func TestWispFixtureBundle(t *testing.T) {
	t.Parallel()
	root := wispFixtureRoot(t)
	manifestPath := filepath.Join(root, "manifest.json")
	data, err := os.ReadFile(manifestPath)
	if err != nil {
		t.Fatalf("read fixture manifest: %v", err)
	}
	pubB64, err := os.ReadFile(filepath.Join(root, "keys", "public.b64"))
	if err != nil {
		t.Fatalf("read public key: %v", err)
	}
	keyIDBytes, err := os.ReadFile(filepath.Join(root, "keys", "key-id.txt"))
	if err != nil {
		t.Fatalf("read key id: %v", err)
	}
	keyID := strings.TrimSpace(string(keyIDBytes))
	pub, err := base64.StdEncoding.DecodeString(strings.TrimSpace(string(pubB64)))
	if err != nil || len(pub) != ed25519.PublicKeySize {
		t.Fatalf("public key: %v len=%d", err, len(pub))
	}
	policy := Policy{
		SupportedProtocol: "1.0.0",
		TrustedKeys: map[string]map[string]ed25519.PublicKey{
			"grok-insider": {keyID: ed25519.PublicKey(pub)},
		},
		AllowedCapabilities: map[string]struct{}{"computer-use.observe": {}},
		PublisherTrust:      map[string]string{"grok-insider": "first-party"},
	}
	verified, err := Verify(data, policy)
	if err != nil {
		t.Fatalf("Verify fixture: %v", err)
	}
	if verified.ID != "desktop.grok.wisp" {
		t.Fatalf("id = %q", verified.ID)
	}
	if verified.Signature.Algorithm != "ed25519" {
		t.Fatalf("algorithm = %q", verified.Signature.Algorithm)
	}
	// Bundle files required for staging must resolve inside the fixture root.
	for _, rel := range []string{"adapter.json", "config.schema.json", "bin/adapter"} {
		resolved, err := ResolveBundlePath(root, rel)
		if err != nil {
			t.Fatalf("ResolveBundlePath %s: %v", rel, err)
		}
		if _, err := os.Stat(resolved); err != nil {
			t.Fatalf("bundle file %s: %v", rel, err)
		}
	}
}

func TestWispFixtureRejectsUnsignedStableManifest(t *testing.T) {
	t.Parallel()
	// Stable channel + algorithm none must fail (signed lifecycle requirement).
	manifest, _, policy := validManifest(t)
	manifest.UpdateChannel = "stable"
	manifest.Signature = Signature{Algorithm: "none", KeyID: nil, Value: nil}
	assertVerificationCode(t, verifyResult(marshalManifest(t, manifest), policy), CodeUnsignedRelease)
}

func wispFixtureRoot(t *testing.T) string {
	t.Helper()
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("caller")
	}
	// native/windows-vm-service/manifestverify -> repo root
	return filepath.Clean(filepath.Join(filepath.Dir(file), "..", "..", "..", "integrations", "testdata", "wisp-signed"))
}
