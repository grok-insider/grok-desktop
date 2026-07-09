//go:build !windows

package transport

import (
	"context"
	"testing"
)

func TestMemoryListenerCarriesAuthenticatedIdentity(t *testing.T) {
	listener := NewMemoryListener(1)
	defer listener.Close()
	client, err := listener.DialContext(context.Background(), PeerIdentity{UserSID: "S-1-5-21-123"})
	if err != nil {
		t.Fatalf("DialContext: %v", err)
	}
	defer client.Close()
	server, err := listener.Accept()
	if err != nil {
		t.Fatalf("Accept: %v", err)
	}
	defer server.Close()
	identity, err := server.AuthenticatePeer()
	if err != nil {
		t.Fatalf("AuthenticatePeer: %v", err)
	}
	if got := identity.UserSID; got != "S-1-5-21-123" {
		t.Fatalf("peer SID = %q", got)
	}
}

func TestLoopbackListenerRejectsNonLoopbackEndpoint(t *testing.T) {
	_, err := Listen(Config{Endpoint: "0.0.0.0:0", DevelopmentPeerSID: "S-1-5-21-123"})
	if err == nil {
		t.Fatal("Listen accepted a non-loopback endpoint")
	}
}
