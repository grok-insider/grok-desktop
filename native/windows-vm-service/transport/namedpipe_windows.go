//go:build windows

package transport

import (
	"fmt"
	"net"
	"runtime"
	"syscall"
	"unsafe"

	"github.com/Microsoft/go-winio"
	"golang.org/x/sys/windows"
)

var (
	advapi32                       = windows.NewLazySystemDLL("advapi32.dll")
	procImpersonateNamedPipeClient = advapi32.NewProc("ImpersonateNamedPipeClient")
)

type namedPipeListener struct {
	net.Listener
	qualifier *desktopClientQualifier
}

type namedPipeConn struct {
	net.Conn
	qualifier *desktopClientQualifier
}

type tokenStatistics struct {
	TokenID            windows.LUID
	AuthenticationID   windows.LUID
	ExpirationTime     int64
	TokenType          uint32
	ImpersonationLevel uint32
	DynamicCharged     uint32
	DynamicAvailable   uint32
	GroupCount         uint32
	PrivilegeCount     uint32
	ModifiedID         windows.LUID
}

func Listen(config Config) (Listener, error) {
	if config.Endpoint != "" && config.Endpoint != ServicePipeName {
		return nil, fmt.Errorf("the production named-pipe endpoint is fixed")
	}
	bufferSize := config.MaxMessageBytes
	if bufferSize < 4096 {
		bufferSize = 4096
	}
	if bufferSize > 1<<20 {
		bufferSize = 1 << 20
	}
	listener, err := winio.ListenPipe(ServicePipeName, &winio.PipeConfig{
		SecurityDescriptor: servicePipeSDDL,
		MessageMode:        false,
		InputBufferSize:    int32(bufferSize),
		OutputBufferSize:   int32(bufferSize),
	})
	if err != nil {
		return nil, fmt.Errorf("listen on authenticated service pipe: %w", err)
	}
	qualifier, err := newDesktopClientQualifier()
	if err != nil {
		_ = listener.Close()
		return nil, fmt.Errorf("initialize desktop process qualification: %w", err)
	}
	return &namedPipeListener{Listener: listener, qualifier: qualifier}, nil
}

func (l *namedPipeListener) Accept() (Conn, error) {
	connection, err := l.Listener.Accept()
	if err != nil {
		return nil, err
	}
	return &namedPipeConn{Conn: connection, qualifier: l.qualifier}, nil
}

// AuthenticatePeer must be called after every request frame. Windows binds the
// identification token to the security context of the data just read, which is
// the proof of possession used by the service rather than any wire field.
func (c *namedPipeConn) AuthenticatePeer() (PeerIdentity, error) {
	identity, err := authenticatedPipeIdentity(c.Conn)
	if err != nil {
		return PeerIdentity{}, err
	}
	if err := ValidatePeerIdentity(identity, false); err != nil {
		return PeerIdentity{}, fmt.Errorf("named-pipe peer is not an eligible interactive user")
	}
	handleProvider, ok := c.Conn.(interface{ Fd() uintptr })
	if !ok || c.qualifier == nil {
		return PeerIdentity{}, fmt.Errorf("named pipe cannot qualify its client process")
	}
	process, err := c.qualifier.identify(windows.Handle(handleProvider.Fd()))
	if err != nil {
		return PeerIdentity{}, err
	}
	identity.ClientProcessID = process.processID
	identity.ClientProcessStart = process.startedAt
	identity.PackagedDesktopQualified = process.packaged
	identity.GuestControlQualified = process.guestGrant
	return identity, nil
}

func authenticatedPipeIdentity(connection net.Conn) (identity PeerIdentity, resultErr error) {
	handleProvider, ok := connection.(interface{ Fd() uintptr })
	if !ok {
		return PeerIdentity{}, fmt.Errorf("named pipe does not expose its Windows handle")
	}

	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	// Production clients must grant SecurityIdentification only. Between this
	// call and RevertToSelf the service may query the token but must never access
	// a resource under it.
	if err := impersonateNamedPipeClient(windows.Handle(handleProvider.Fd())); err != nil {
		return PeerIdentity{}, fmt.Errorf("identify named-pipe client: %w", err)
	}
	defer func() {
		if err := windows.RevertToSelf(); resultErr == nil && err != nil {
			resultErr = fmt.Errorf("revert named-pipe impersonation: %w", err)
		}
	}()

	var token windows.Token
	if err := windows.OpenThreadToken(windows.CurrentThread(), windows.TOKEN_QUERY, true, &token); err != nil {
		return PeerIdentity{}, fmt.Errorf("open identified client token: %w", err)
	}
	defer token.Close()
	return identityFromToken(token)
}

func identityFromToken(token windows.Token) (PeerIdentity, error) {
	user, err := token.GetTokenUser()
	if err != nil {
		return PeerIdentity{}, fmt.Errorf("read identified client identity: %w", err)
	}
	groups, err := token.GetTokenGroups()
	if err != nil {
		return PeerIdentity{}, fmt.Errorf("read identified client groups: %w", err)
	}
	var sessionID uint32
	if err := tokenScalar(token, windows.TokenSessionId, unsafe.Pointer(&sessionID), unsafe.Sizeof(sessionID)); err != nil {
		return PeerIdentity{}, fmt.Errorf("read identified client session: %w", err)
	}
	var statistics tokenStatistics
	if err := tokenScalar(token, windows.TokenStatistics, unsafe.Pointer(&statistics), unsafe.Sizeof(statistics)); err != nil {
		return PeerIdentity{}, fmt.Errorf("read identified client proof: %w", err)
	}
	if statistics.TokenType != windows.TokenImpersonation {
		return PeerIdentity{}, fmt.Errorf("named-pipe client token is not an impersonation token")
	}

	interactive := false
	network := false
	for _, group := range groups.AllGroups() {
		if group.Sid == nil || group.Attributes&windows.SE_GROUP_ENABLED == 0 {
			continue
		}
		switch group.Sid.String() {
		case "S-1-5-4", "S-1-5-14":
			interactive = true
		case "S-1-5-2":
			network = true
		}
	}
	return PeerIdentity{
		UserSID:            user.User.Sid.String(),
		SessionID:          sessionID,
		AuthenticationID:   luidValue(statistics.AuthenticationID),
		ImpersonationLevel: statistics.ImpersonationLevel,
		Interactive:        interactive,
		Network:            network,
		Method:             AuthenticationWindowsNamedPipe,
	}, nil
}

func tokenScalar(token windows.Token, class uint32, destination unsafe.Pointer, size uintptr) error {
	var returned uint32
	if err := windows.GetTokenInformation(token, class, (*byte)(destination), uint32(size), &returned); err != nil {
		return err
	}
	if returned != uint32(size) {
		return fmt.Errorf("token information has an unexpected size")
	}
	return nil
}

func luidValue(value windows.LUID) uint64 {
	return uint64(uint32(value.HighPart))<<32 | uint64(value.LowPart)
}

func impersonateNamedPipeClient(pipe windows.Handle) error {
	result, _, callErr := procImpersonateNamedPipeClient.Call(uintptr(pipe))
	if result != 0 {
		return nil
	}
	if callErr != nil && callErr != syscall.Errno(0) {
		return callErr
	}
	return syscall.EINVAL
}

var _ Listener = (*namedPipeListener)(nil)
var _ Conn = (*namedPipeConn)(nil)
