package main

import (
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"testing"
)

func TestDecodeCompiledGuestCatalogTrustRequiresMatchingBinding(t *testing.T) {
	previousTrust, previousBinding := guestCatalogTrust, guestCatalogTrustBinding
	t.Cleanup(func() {
		guestCatalogTrust, guestCatalogTrustBinding = previousTrust, previousBinding
	})
	raw := `{"release":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}`
	guestCatalogTrust = base64.StdEncoding.EncodeToString([]byte(raw))
	digest := sha256.Sum256([]byte(guestCatalogTrust))
	guestCatalogTrustBinding = guestCatalogTrustBindingPrefix + hex.EncodeToString(digest[:])
	decoded, err := decodeCompiledGuestCatalogTrust()
	if err != nil || decoded != raw {
		t.Fatalf("decodeCompiledGuestCatalogTrust() = %q, %v", decoded, err)
	}
	guestCatalogTrustBinding = guestCatalogTrustBindingPrefix + "0"
	if _, err := decodeCompiledGuestCatalogTrust(); err == nil {
		t.Fatal("mismatched compiled trust binding was accepted")
	}
}
