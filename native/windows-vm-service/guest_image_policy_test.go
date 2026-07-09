package vmservice

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestVerifyGuestImageCatalogAcceptsSignedOfficialInventory(t *testing.T) {
	publicKey, privateKey := newGuestCatalogTestKey(t)
	data := signedGuestCatalog(t, privateKey, "release-2026", "x64", 42, []OfficialGuestImage{
		testOfficialImage("nixos", "grok-guest-1.2.3-x64.vhdx", []byte("official-vhdx")),
	})
	policy, err := VerifyGuestImageCatalog(data, "x64", testGuestCatalogTrust(publicKey))
	if err != nil {
		t.Fatalf("VerifyGuestImageCatalog: %v", err)
	}
	image, exists := policy.image("nixos")
	if !exists || image.Version != "1.2.3" || image.StagingName != "grok-guest-1.2.3-x64.vhdx" {
		t.Fatalf("verified inventory = %#v, %v", image, exists)
	}
}

func TestGuestImageCatalogSigningBytesWireCompatibility(t *testing.T) {
	catalog := GuestImageCatalog{
		SchemaVersion: 1,
		Product:       "grok-desktop-guest",
		Architecture:  "x64",
		Sequence:      7,
		Images: []OfficialGuestImage{{
			ID: "grok-guest-1.2.3", Version: "1.2.3", StagingName: "grok-guest.vhdx",
			SHA256: strings.Repeat("a", 64), SizeBytes: 123,
		}},
		Signature: GuestCatalogSignature{Algorithm: "ed25519", KeyID: "release-2026", Value: "excluded"},
	}
	encoded, err := GuestImageCatalogSigningBytes(catalog)
	if err != nil {
		t.Fatal(err)
	}
	want := `{"schemaVersion":1,"product":"grok-desktop-guest","architecture":"x64","sequence":7,"images":[{"id":"grok-guest-1.2.3","version":"1.2.3","stagingName":"grok-guest.vhdx","sha256":"` + strings.Repeat("a", 64) + `","sizeBytes":123}],"signature":{"algorithm":"ed25519","keyId":"release-2026"}}` + "\n"
	if string(encoded) != want {
		t.Fatalf("signing bytes = %q, want %q", encoded, want)
	}
}

func TestVerifyGuestImageCatalogRejectsMetadataForgery(t *testing.T) {
	publicKey, privateKey := newGuestCatalogTestKey(t)
	data := signedGuestCatalog(t, privateKey, "release-2026", "x64", 7, []OfficialGuestImage{
		testOfficialImage("nixos", "grok-guest.vhdx", []byte("official-vhdx")),
	})
	forged := strings.Replace(string(data), `"sequence":7`, `"sequence":8`, 1)
	if forged == string(data) {
		t.Fatal("test did not modify signed metadata")
	}
	_, err := VerifyGuestImageCatalog([]byte(forged), "x64", testGuestCatalogTrust(publicKey))
	assertGuestPolicyServiceCode(t, err, CodePermissionDenied)
}

func TestVerifyGuestImageCatalogRejectsWrongArchitecture(t *testing.T) {
	publicKey, privateKey := newGuestCatalogTestKey(t)
	data := signedGuestCatalog(t, privateKey, "release-2026", "arm64", 7, []OfficialGuestImage{
		testOfficialImage("nixos", "grok-guest.vhdx", []byte("official-vhdx")),
	})
	_, err := VerifyGuestImageCatalog(data, "x64", testGuestCatalogTrust(publicKey))
	assertGuestPolicyServiceCode(t, err, CodePermissionDenied)
}

func TestVerifyGuestImageCatalogRejectsUnknownOrDuplicateFields(t *testing.T) {
	publicKey, privateKey := newGuestCatalogTestKey(t)
	data := signedGuestCatalog(t, privateKey, "release-2026", "x64", 7, []OfficialGuestImage{
		testOfficialImage("nixos", "grok-guest.vhdx", []byte("official-vhdx")),
	})
	unknown := strings.Replace(string(data), `"product":"grok-desktop-guest"`, `"product":"grok-desktop-guest","extra":true`, 1)
	_, err := VerifyGuestImageCatalog([]byte(unknown), "x64", testGuestCatalogTrust(publicKey))
	assertGuestPolicyServiceCode(t, err, CodePermissionDenied)
	duplicate := strings.Replace(string(data), `"sequence":7`, `"sequence":7,"sequence":7`, 1)
	_, err = VerifyGuestImageCatalog([]byte(duplicate), "x64", testGuestCatalogTrust(publicKey))
	assertGuestPolicyServiceCode(t, err, CodePermissionDenied)
}

func TestVerifyGuestImageCatalogRejectsSignedNoncanonicalInventory(t *testing.T) {
	publicKey, privateKey := newGuestCatalogTestKey(t)
	tests := []OfficialGuestImage{
		{ID: "nixos", Version: "1.2.3-alpha..1", StagingName: "grok-guest.vhdx", SHA256: strings.Repeat("a", 64), SizeBytes: 4},
		{ID: "con", Version: "1.2.3", StagingName: "grok-guest.vhdx", SHA256: strings.Repeat("a", 64), SizeBytes: 4},
		{ID: "nixos", Version: "1.2.3", StagingName: "con.vhdx", SHA256: strings.Repeat("a", 64), SizeBytes: 4},
	}
	for _, image := range tests {
		data := signedGuestCatalog(t, privateKey, "release-2026", "x64", 7, []OfficialGuestImage{image})
		_, err := VerifyGuestImageCatalog(data, "x64", testGuestCatalogTrust(publicKey))
		assertGuestPolicyServiceCode(t, err, CodePermissionDenied)
	}
}

func TestGuestImagePolicyRollbackRejectsDowngradeAndEquivocation(t *testing.T) {
	publicKey, privateKey := newGuestCatalogTestKey(t)
	trust := testGuestCatalogTrust(publicKey)
	policy := verifiedGuestCatalogPolicy(t, privateKey, trust, 10, []byte("v10"))
	root := filepath.Join(t.TempDir(), "service-data")
	if err := EnforceGuestImagePolicyRollback(root, policy); err != nil {
		t.Fatalf("record policy: %v", err)
	}
	if err := EnforceGuestImagePolicyRollback(root, policy); err != nil {
		t.Fatalf("accept identical policy: %v", err)
	}
	downgrade := verifiedGuestCatalogPolicy(t, privateKey, trust, 9, []byte("v9"))
	assertGuestPolicyServiceCode(t, EnforceGuestImagePolicyRollback(root, downgrade), CodePermissionDenied)
	equivocation := verifiedGuestCatalogPolicy(t, privateKey, trust, 10, []byte("different-v10"))
	assertGuestPolicyServiceCode(t, EnforceGuestImagePolicyRollback(root, equivocation), CodePermissionDenied)
	upgrade := verifiedGuestCatalogPolicy(t, privateKey, trust, 11, []byte("v11"))
	if err := EnforceGuestImagePolicyRollback(root, upgrade); err != nil {
		t.Fatalf("accept higher sequence: %v", err)
	}
}

func TestLoadGuestImagePolicyUsesFixedCatalogPath(t *testing.T) {
	publicKey, privateKey := newGuestCatalogTestKey(t)
	releaseRoot := t.TempDir()
	catalogRoot := filepath.Join(releaseRoot, "catalog")
	if err := os.Mkdir(catalogRoot, 0o700); err != nil {
		t.Fatal(err)
	}
	data := signedGuestCatalog(t, privateKey, "release-2026", "x64", 1, []OfficialGuestImage{
		testOfficialImage("nixos", "grok-guest.vhdx", []byte("official-vhdx")),
	})
	if err := os.WriteFile(filepath.Join(catalogRoot, "components.json"), data, 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := LoadGuestImagePolicy(releaseRoot, "x64", testGuestCatalogTrust(publicKey)); err != nil {
		t.Fatalf("LoadGuestImagePolicy: %v", err)
	}
}

func TestParseGuestImageTrustRejectsPrivateOrNonCanonicalMaterial(t *testing.T) {
	publicKey, privateKey := newGuestCatalogTestKey(t)
	valid := `{"release":"` + base64.StdEncoding.EncodeToString(publicKey) + `"}`
	if _, err := ParseGuestImageTrust(valid); err != nil {
		t.Fatalf("ParseGuestImageTrust: %v", err)
	}
	private := `{"release":"` + base64.StdEncoding.EncodeToString(privateKey) + `"}`
	if _, err := ParseGuestImageTrust(private); err == nil {
		t.Fatal("private key bytes were accepted as a public trust anchor")
	}
	if _, err := ParseGuestImageTrust(`{"release":"not-base64"}`); err == nil {
		t.Fatal("malformed public key was accepted")
	}
}

func newGuestCatalogTestKey(t *testing.T) (ed25519.PublicKey, ed25519.PrivateKey) {
	t.Helper()
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	return publicKey, privateKey
}

func testGuestCatalogTrust(publicKey ed25519.PublicKey) GuestImageTrust {
	return GuestImageTrust{keys: map[string]ed25519.PublicKey{"release-2026": append(ed25519.PublicKey(nil), publicKey...)}}
}

func testOfficialImage(id, stagingName string, contents []byte) OfficialGuestImage {
	digest := sha256.Sum256(contents)
	return OfficialGuestImage{
		ID: id, Version: "1.2.3", StagingName: stagingName,
		SHA256: hex.EncodeToString(digest[:]), SizeBytes: int64(len(contents)),
	}
}

func signedGuestCatalog(
	t *testing.T,
	privateKey ed25519.PrivateKey,
	keyID, architecture string,
	sequence uint64,
	images []OfficialGuestImage,
) []byte {
	t.Helper()
	catalog := GuestImageCatalog{
		SchemaVersion: guestImageCatalogVersion,
		Product:       "grok-desktop-guest",
		Architecture:  architecture,
		Sequence:      sequence,
		Images:        images,
		Signature:     GuestCatalogSignature{Algorithm: "ed25519", KeyID: keyID},
	}
	signingBytes, err := GuestImageCatalogSigningBytes(catalog)
	if err != nil {
		t.Fatal(err)
	}
	catalog.Signature.Value = base64.StdEncoding.EncodeToString(ed25519.Sign(privateKey, signingBytes))
	data, err := json.Marshal(catalog)
	if err != nil {
		t.Fatal(err)
	}
	return append(data, '\n')
}

func verifiedGuestCatalogPolicy(
	t *testing.T,
	privateKey ed25519.PrivateKey,
	trust GuestImageTrust,
	sequence uint64,
	contents []byte,
) *GuestImagePolicy {
	t.Helper()
	data := signedGuestCatalog(t, privateKey, "release-2026", "x64", sequence, []OfficialGuestImage{
		testOfficialImage("nixos", "grok-guest.vhdx", contents),
	})
	policy, err := VerifyGuestImageCatalog(data, "x64", trust)
	if err != nil {
		t.Fatal(err)
	}
	return policy
}

func assertGuestPolicyServiceCode(t *testing.T, err error, code ErrorCode) {
	t.Helper()
	var serviceErr *Error
	if !errors.As(err, &serviceErr) || serviceErr.Code != code {
		t.Fatalf("error = %v, want service code %q", err, code)
	}
}
