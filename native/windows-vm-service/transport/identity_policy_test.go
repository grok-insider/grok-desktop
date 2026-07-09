package transport

import "testing"

func TestValidatePeerIdentityProductionPolicy(t *testing.T) {
	valid := PeerIdentity{
		UserSID: "S-1-5-21-1000-1001-1002-1003", SessionID: 2,
		AuthenticationID: 0x11223344, ImpersonationLevel: SecurityIdentification,
		Interactive: true, Method: AuthenticationWindowsNamedPipe,
	}
	if err := ValidatePeerIdentity(valid, false); err != nil {
		t.Fatalf("valid identity rejected: %v", err)
	}

	tests := []struct {
		name string
		edit func(*PeerIdentity)
	}{
		{"anonymous", func(identity *PeerIdentity) { identity.UserSID = "S-1-5-7" }},
		{"local system", func(identity *PeerIdentity) { identity.UserSID = "S-1-5-18" }},
		{"virtual service", func(identity *PeerIdentity) { identity.UserSID = "S-1-5-80-123" }},
		{"session zero", func(identity *PeerIdentity) { identity.SessionID = 0 }},
		{"missing logon proof", func(identity *PeerIdentity) { identity.AuthenticationID = 0 }},
		{"anonymous token", func(identity *PeerIdentity) { identity.ImpersonationLevel = 0 }},
		{"impersonation token", func(identity *PeerIdentity) { identity.ImpersonationLevel = 2 }},
		{"delegation token", func(identity *PeerIdentity) { identity.ImpersonationLevel = 3 }},
		{"non interactive", func(identity *PeerIdentity) { identity.Interactive = false }},
		{"network", func(identity *PeerIdentity) { identity.Network = true }},
		{"development", func(identity *PeerIdentity) { identity.Method = AuthenticationDevelopment }},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			candidate := valid
			test.edit(&candidate)
			if err := ValidatePeerIdentity(candidate, false); err == nil {
				t.Fatal("identity was accepted")
			}
		})
	}
}

func TestValidatePeerIdentityDevelopmentIsExplicit(t *testing.T) {
	identity := PeerIdentity{UserSID: "S-1-5-21-1000-1001-1002-1003", Method: AuthenticationDevelopment}
	if err := ValidatePeerIdentity(identity, true); err != nil {
		t.Fatalf("development identity rejected: %v", err)
	}
	if err := ValidatePeerIdentity(identity, false); err == nil {
		t.Fatal("development identity accepted by production policy")
	}
}

func TestSamePrincipalIncludesLogonSessionProof(t *testing.T) {
	left := PeerIdentity{
		UserSID: "S-1-5-21-1000", SessionID: 3, AuthenticationID: 91,
		Method: AuthenticationWindowsNamedPipe,
	}
	right := left
	right.UserSID = "s-1-5-21-1000"
	if !SamePrincipal(left, right) {
		t.Fatal("same principal was not recognized")
	}
	right.AuthenticationID++
	if SamePrincipal(left, right) {
		t.Fatal("different logon authentication IDs were treated as one principal")
	}
	right = left
	right.GuestControlQualified = true
	if SamePrincipal(left, right) {
		t.Fatal("a guest-control qualification change was accepted on an established connection")
	}
}
