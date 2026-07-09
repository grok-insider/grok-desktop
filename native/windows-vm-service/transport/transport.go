package transport

import (
	"fmt"
	"net"
	"regexp"
	"strings"
)

type AuthenticationMethod string

const (
	AuthenticationWindowsNamedPipe AuthenticationMethod = "windows-named-pipe-identification"
	AuthenticationDevelopment      AuthenticationMethod = "development-loopback"
)

const SecurityIdentification = uint32(1)

var transportSIDPattern = regexp.MustCompile(`(?i)^s-1-[0-9]+(?:-[0-9]+)+$`)

type PeerIdentity struct {
	UserSID            string
	SessionID          uint32
	AuthenticationID   uint64
	ImpersonationLevel uint32
	Interactive        bool
	Network            bool
	ClientProcessID    uint32
	ClientProcessStart uint64
	// PackagedDesktopQualified proves process path and same-package identity,
	// but does not by itself grant guest control. A separate daemon
	// proof-of-possession exchange must promote GuestControlQualified.
	PackagedDesktopQualified bool
	// GuestControlQualified is set only after packaged process qualification
	// and the daemon proof-of-possession exchange both succeed. The current
	// production named-pipe adapter deliberately leaves it false.
	GuestControlQualified bool
	Method                AuthenticationMethod
}

// ValidatePeerIdentity applies the platform-independent portion of the
// production named-pipe identity policy. The Windows adapter fills this value
// exclusively from an identification-only access token after every frame read.
func ValidatePeerIdentity(identity PeerIdentity, allowDevelopment bool) error {
	if !transportSIDPattern.MatchString(identity.UserSID) {
		return fmt.Errorf("peer identity is not a canonical Windows user identity")
	}
	canonical := strings.ToUpper(identity.UserSID)
	switch canonical {
	case "S-1-5-7", "S-1-5-18", "S-1-5-19", "S-1-5-20":
		return fmt.Errorf("service and anonymous identities are not permitted")
	}
	if strings.HasPrefix(canonical, "S-1-5-80-") || strings.HasPrefix(canonical, "S-1-5-82-") {
		return fmt.Errorf("virtual service identities are not permitted")
	}

	if identity.Method == AuthenticationDevelopment && allowDevelopment {
		return nil
	}
	if identity.Method != AuthenticationWindowsNamedPipe {
		return fmt.Errorf("peer authentication method is not permitted")
	}
	if identity.SessionID == 0 {
		return fmt.Errorf("session zero identities are not permitted")
	}
	if identity.AuthenticationID == 0 {
		return fmt.Errorf("peer token has no logon proof")
	}
	if identity.ImpersonationLevel != SecurityIdentification {
		return fmt.Errorf("peer token must grant identification only")
	}
	if !identity.Interactive || identity.Network {
		return fmt.Errorf("only local interactive logons are permitted")
	}
	return nil
}

// SamePrincipal binds a connection to the first authenticated logon while
// still requiring the operating system to authenticate every later frame.
func SamePrincipal(left, right PeerIdentity) bool {
	return strings.EqualFold(left.UserSID, right.UserSID) &&
		left.SessionID == right.SessionID &&
		left.AuthenticationID == right.AuthenticationID &&
		left.ClientProcessID == right.ClientProcessID &&
		left.ClientProcessStart == right.ClientProcessStart &&
		left.PackagedDesktopQualified == right.PackagedDesktopQualified &&
		left.GuestControlQualified == right.GuestControlQualified &&
		left.Method == right.Method
}

// PrincipalCacheKey keeps replay results scoped to the authenticated logon and
// its authorization level. It is an in-memory key, not a stable identity or a
// value suitable for logs or durable storage.
func PrincipalCacheKey(identity PeerIdentity) string {
	return fmt.Sprintf(
		"%s\x00%d\x00%d\x00%d\x00%d\x00%d\x00%t\x00%t\x00%s",
		strings.ToUpper(identity.UserSID),
		identity.SessionID,
		identity.AuthenticationID,
		identity.ImpersonationLevel,
		identity.ClientProcessID,
		identity.ClientProcessStart,
		identity.PackagedDesktopQualified,
		identity.GuestControlQualified,
		identity.Method,
	)
}

type Conn interface {
	net.Conn
	AuthenticatePeer() (PeerIdentity, error)
}

type Listener interface {
	Accept() (Conn, error)
	Close() error
	Addr() net.Addr
}

type Config struct {
	Endpoint           string
	DevelopmentPeerSID string
	MaxMessageBytes    int
}

type authenticatedConn struct {
	net.Conn
	identity PeerIdentity
}

func (c *authenticatedConn) AuthenticatePeer() (PeerIdentity, error) {
	return c.identity, nil
}
