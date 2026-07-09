//go:build linux

package workspace

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"os/user"
	"path/filepath"
	"strconv"
	"sync"
	"time"

	"github.com/grok-insider/grok-desktop/guest/runner/internal/strictjson"
	"github.com/mdlayher/vsock"
	"golang.org/x/sys/unix"
)

const maxBrokerConnections = 8

type BrokerConfig struct {
	SocketPath    string
	WorkspaceRoot string
	RunnerUser    string
	RunnerGroup   string
}

type Broker struct {
	config    BrokerConfig
	runnerUID uint32
	runnerGID uint32
	rootFD    int
	listener  *net.UnixListener

	mountMu sync.Mutex
}

func NewBroker(config BrokerConfig) (*Broker, error) {
	if !filepath.IsAbs(config.SocketPath) || filepath.Clean(config.SocketPath) != config.SocketPath ||
		!filepath.IsAbs(config.WorkspaceRoot) || filepath.Clean(config.WorkspaceRoot) != config.WorkspaceRoot {
		return nil, errors.New("workspace broker path is invalid")
	}
	runner, err := user.Lookup(config.RunnerUser)
	if err != nil {
		return nil, errors.New("workspace runner identity is unavailable")
	}
	group, err := user.LookupGroup(config.RunnerGroup)
	if err != nil {
		return nil, errors.New("workspace runner group is unavailable")
	}
	uid, err := parseIdentity(runner.Uid)
	if err != nil {
		return nil, err
	}
	gid, err := parseIdentity(group.Gid)
	if err != nil {
		return nil, err
	}
	rootFD, err := unix.Open(config.WorkspaceRoot, unix.O_RDONLY|unix.O_DIRECTORY|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0)
	if err != nil {
		return nil, errors.New("workspace root could not be opened")
	}
	var stat unix.Stat_t
	if err := unix.Fstat(rootFD, &stat); err != nil || stat.Mode&unix.S_IFMT != unix.S_IFDIR || stat.Uid != 0 || stat.Mode&0o022 != 0 {
		unix.Close(rootFD)
		return nil, errors.New("workspace root ownership or permissions are unsafe")
	}
	return &Broker{config: config, runnerUID: uid, runnerGID: gid, rootFD: rootFD}, nil
}

func (broker *Broker) Listen() error {
	if broker.listener != nil {
		return errors.New("workspace broker is already listening")
	}
	if info, err := os.Lstat(broker.config.SocketPath); err == nil {
		stat, ok := info.Sys().(*unix.Stat_t)
		if !ok || info.Mode()&os.ModeSocket == 0 || stat.Uid != 0 {
			return errors.New("existing workspace broker socket is unsafe")
		}
		if err := os.Remove(broker.config.SocketPath); err != nil {
			return errors.New("stale workspace broker socket could not be removed")
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return errors.New("workspace broker socket could not be inspected")
	}
	listener, err := net.ListenUnix("unix", &net.UnixAddr{Name: broker.config.SocketPath, Net: "unix"})
	if err != nil {
		return errors.New("workspace broker socket could not be created")
	}
	if err := os.Chmod(broker.config.SocketPath, 0o660); err != nil || os.Chown(broker.config.SocketPath, 0, int(broker.runnerGID)) != nil {
		listener.Close()
		os.Remove(broker.config.SocketPath)
		return errors.New("workspace broker socket permissions could not be set")
	}
	broker.listener = listener
	return nil
}

func (broker *Broker) Serve(ctx context.Context) error {
	if broker.listener == nil {
		return errors.New("workspace broker is not listening")
	}
	go func() {
		<-ctx.Done()
		_ = broker.listener.Close()
	}()
	capacity := make(chan struct{}, maxBrokerConnections)
	var active sync.WaitGroup
	defer active.Wait()
	for {
		connection, err := broker.listener.AcceptUnix()
		if err != nil {
			if ctx.Err() != nil || errors.Is(err, net.ErrClosed) {
				return nil
			}
			return errors.New("workspace broker listener failed")
		}
		if !broker.authorized(connection) {
			connection.Close()
			continue
		}
		select {
		case capacity <- struct{}{}:
			active.Add(1)
			go func() {
				defer active.Done()
				defer func() { <-capacity }()
				broker.handle(ctx, connection)
			}()
		default:
			connection.Close()
		}
	}
}

func (broker *Broker) Close() error {
	if broker.listener != nil {
		_ = broker.listener.Close()
		broker.listener = nil
	}
	_ = os.Remove(broker.config.SocketPath)
	if broker.rootFD >= 0 {
		err := unix.Close(broker.rootFD)
		broker.rootFD = -1
		return err
	}
	return nil
}

func (broker *Broker) authorized(connection *net.UnixConn) bool {
	raw, err := connection.SyscallConn()
	if err != nil {
		return false
	}
	var credentials *unix.Ucred
	var socketError error
	if err := raw.Control(func(fd uintptr) {
		credentials, socketError = unix.GetsockoptUcred(int(fd), unix.SOL_SOCKET, unix.SO_PEERCRED)
	}); err != nil || socketError != nil || credentials == nil {
		return false
	}
	return credentials.Uid == broker.runnerUID && credentials.Gid == broker.runnerGID
}

func (broker *Broker) handle(ctx context.Context, connection *net.UnixConn) {
	defer connection.Close()
	_ = connection.SetDeadline(time.Now().Add(15 * time.Second))
	line, err := readLine(bufio.NewReaderSize(connection, maximumFrameBytes))
	if err != nil {
		return
	}
	var message request
	if err := strictjson.Decode(line, maximumFrameBytes, &message); err != nil ||
		message.Protocol != protocol || message.Type != "request" ||
		!identifierPattern.MatchString(message.ID) || message.Method != "prepare" ||
		validateMount(broker.config.WorkspaceRoot, message.MountID, message.Path) != nil {
		broker.writeResponse(connection, response{Protocol: protocol, Type: "response", ID: "invalid", Error: &responseError{Code: "INVALID_ARGUMENT"}})
		return
	}
	if err := broker.prepare(ctx, message.MountID, message.Path); err != nil {
		broker.writeResponse(connection, response{Protocol: protocol, Type: "response", ID: message.ID, Error: &responseError{Code: "MOUNT_REJECTED"}})
		return
	}
	broker.writeResponse(connection, response{Protocol: protocol, Type: "response", ID: message.ID, OK: true})
}

func (*Broker) writeResponse(connection *net.UnixConn, message response) {
	data, err := json.Marshal(message)
	if err == nil && len(data)+1 <= maximumFrameBytes {
		_, _ = connection.Write(append(data, '\n'))
	}
}

func (broker *Broker) prepare(ctx context.Context, mountID, target string) error {
	broker.mountMu.Lock()
	defer broker.mountMu.Unlock()
	if err := ctx.Err(); err != nil {
		return err
	}
	created, err := broker.ensureMountPoint(mountID)
	if err != nil {
		return err
	}
	if mountedReadOnly9P(target) {
		return nil
	}
	entries, err := os.ReadDir(target)
	if err != nil || len(entries) != 0 {
		return errors.New("workspace mount point is not empty")
	}
	connection, err := vsock.Dial(vsock.Host, plan9Port, nil)
	if err != nil {
		if created {
			_ = unix.Unlinkat(broker.rootFD, mountID, unix.AT_REMOVEDIR)
		}
		return errors.New("Plan9 service is unavailable")
	}
	file, err := duplicateSocket(connection)
	connection.Close()
	if err != nil {
		return err
	}
	defer file.Close()
	_ = unix.SetsockoptInt(int(file.Fd()), unix.SOL_SOCKET, unix.SO_RCVBUF, 65536)
	_ = unix.SetsockoptInt(int(file.Fd()), unix.SOL_SOCKET, unix.SO_SNDBUF, 65536)
	data := fmt.Sprintf("trans=fd,rfdno=%d,wfdno=%d,msize=65536,noload,cache=none,access=any,aname=%s", file.Fd(), file.Fd(), mountID)
	flags := uintptr(unix.MS_RDONLY | unix.MS_NOSUID | unix.MS_NODEV | unix.MS_NOEXEC)
	if err := unix.Mount(target, target, "9p", flags, data); err != nil {
		if created {
			_ = unix.Unlinkat(broker.rootFD, mountID, unix.AT_REMOVEDIR)
		}
		return errors.New("Plan9 workspace could not be mounted")
	}
	if !mountedReadOnly9P(target) {
		_ = unix.Unmount(target, unix.MNT_DETACH)
		return errors.New("Plan9 workspace mount verification failed")
	}
	return nil
}

func (broker *Broker) ensureMountPoint(mountID string) (bool, error) {
	created := false
	if err := unix.Mkdirat(broker.rootFD, mountID, 0o550); err == nil {
		created = true
	} else if !errors.Is(err, unix.EEXIST) {
		return false, errors.New("workspace mount point could not be created")
	}
	fd, err := unix.Openat2(broker.rootFD, mountID, &unix.OpenHow{
		Flags: unix.O_RDONLY | unix.O_DIRECTORY | unix.O_CLOEXEC | unix.O_NOFOLLOW,
		Resolve: unix.RESOLVE_BENEATH | unix.RESOLVE_NO_SYMLINKS |
			unix.RESOLVE_NO_MAGICLINKS,
	})
	if err != nil {
		return false, errors.New("workspace mount point could not be opened safely")
	}
	defer unix.Close(fd)
	var stat unix.Stat_t
	if err := unix.Fstat(fd, &stat); err != nil || stat.Mode&unix.S_IFMT != unix.S_IFDIR || stat.Uid != 0 || stat.Mode&0o022 != 0 {
		return false, errors.New("workspace mount point ownership or permissions are unsafe")
	}
	if created {
		if err := unix.Fchown(fd, 0, int(broker.runnerGID)); err != nil || unix.Fchmod(fd, 0o550) != nil {
			return false, errors.New("workspace mount point permissions could not be set")
		}
	}
	return created, nil
}

func mountedReadOnly9P(path string) bool {
	var stat unix.Statfs_t
	return unix.Statfs(path, &stat) == nil && stat.Type == unix.V9FS_MAGIC && uint64(stat.Flags)&unix.ST_RDONLY != 0
}

func duplicateSocket(connection *vsock.Conn) (*os.File, error) {
	raw, err := connection.SyscallConn()
	if err != nil {
		return nil, errors.New("Plan9 socket descriptor is unavailable")
	}
	duplicated := -1
	var duplicateError error
	if err := raw.Control(func(fd uintptr) {
		duplicated, duplicateError = unix.FcntlInt(fd, unix.F_DUPFD_CLOEXEC, 3)
	}); err != nil || duplicateError != nil || duplicated < 0 {
		return nil, errors.New("Plan9 socket descriptor could not be duplicated")
	}
	return os.NewFile(uintptr(duplicated), "plan9-workspace"), nil
}

func parseIdentity(value string) (uint32, error) {
	parsed, err := strconv.ParseUint(value, 10, 32)
	if err != nil {
		return 0, errors.New("workspace runner identity is invalid")
	}
	return uint32(parsed), nil
}
