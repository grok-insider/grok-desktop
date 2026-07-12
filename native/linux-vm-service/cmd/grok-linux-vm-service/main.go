// Command grok-linux-vm-service is the privileged Linux isolation broker entry
// point (platform ADR 0004). It speaks a bounded JSON-lines protocol over a
// unix domain socket and never returns raw guest endpoints.
//
// Product path operations: get_capabilities, ensure_image, create_vm, start_vm,
// guest_control (grant + runner.health). Peer identity is resolved from
// SO_PEERCRED + /proc/<pid>/exe; client-supplied peerExe is never trusted alone.
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	linuxvmservice "github.com/grok-insider/grok-desktop/native/linux-vm-service"
	"golang.org/x/sys/unix"
)

const (
	wireVersion         = "1.0.0"
	maxFrameBytes       = 128 * 1024
	maxLegacyFrameBytes = 64 * 1024
	maxConnections      = 32
	maxRequestHorizon   = 30 * time.Second
	defaultIOTimeout    = 10 * time.Second
)

type requestEnvelope struct {
	Version   string `json:"version"`
	ID        string `json:"id"`
	Operation string `json:"operation"`
	Deadline  string `json:"deadline"`
	// PeerExe is ignored for authorization; retained only for diagnostics logs.
	PeerExe string          `json:"peerExe,omitempty"`
	Payload json.RawMessage `json:"payload"`
}

type responseEnvelope struct {
	Version string      `json:"version"`
	ID      string      `json:"id"`
	OK      bool        `json:"ok"`
	Result  interface{} `json:"result,omitempty"`
	Error   *wireError  `json:"error,omitempty"`
}

type wireError struct {
	Code      string `json:"code"`
	Message   string `json:"message"`
	Retryable bool   `json:"retryable"`
}

type guestControlPayload struct {
	VmID    string `json:"vmId"`
	Method  string `json:"method"`
	Proof   string `json:"proof"`
	Payload []byte `json:"payload,omitempty"`
}

type ensureImagePayload struct {
	ImageID      string `json:"imageId"`
	RelativePath string `json:"relativePath"`
	SHA256       string `json:"sha256"`
	SizeBytes    int64  `json:"sizeBytes"`
}

type createVmPayload struct {
	VmID      string `json:"vmId"`
	ImageID   string `json:"imageId"`
	VCPUCount uint16 `json:"vcpuCount"`
	MemoryMiB uint32 `json:"memoryMiB"`
}

type startVmPayload struct {
	VmID string `json:"vmId"`
}

type labProc struct {
	alive bool
}

func (p *labProc) Kill() error { p.alive = false; return nil }
func (p *labProc) Alive() bool { return p.alive }

func main() {
	socketPath := envOr("GROK_LINUX_VM_SOCKET", "/run/grok-desktop/linux-vm-service.sock")
	imageRoot := envOr("GROK_LINUX_VM_IMAGE_ROOT", filepath.Join(os.TempDir(), "grok-linux-vm-images"))
	daemonPath := envOr("GROK_LINUX_VM_ALLOWED_DAEMON", "")
	if daemonPath == "" {
		// Production packaging must set GROK_LINUX_VM_ALLOWED_DAEMON to the exact grok-daemon path.
		if self, err := os.Executable(); err == nil {
			// Lab default: allow the current test process binary only when explicitly set.
			_ = self
		}
		cwd, _ := os.Getwd()
		daemonPath = filepath.Join(cwd, "target", "debug", "grok-daemon")
	}
	allowedUID, err := configuredAllowedUID(os.Getenv("GROK_LINUX_VM_ALLOWED_UID"))
	if err != nil {
		log.Fatalf("allowed daemon uid: %v", err)
	}
	if err := os.MkdirAll(imageRoot, 0o700); err != nil {
		log.Fatalf("image root: %v", err)
	}
	socketLock, err := prepareSocketPath(socketPath)
	if err != nil {
		log.Fatalf("socket path: %v", err)
	}
	defer socketLock.Close()

	requireKVM := os.Getenv("GROK_LINUX_VM_REQUIRE_KVM") != "0"
	cfg := linuxvmservice.Config{
		ImageRoot:          imageRoot,
		AllowedDaemonPaths: []string{daemonPath},
		AllowedDaemonUIDs:  []uint32{allowedUID},
		RequireKVM:         requireKVM,
		RunnerHealthHook:   labRunnerHealthHook(),
		Spawn:              labSpawn(),
	}
	if os.Getenv("GROK_LINUX_VM_LAB_SPAWN") == "1" {
		// Lab/orchestrator tests inject a live fake process; never used as Work isolation.
		alive := true
		cfg.KVMPresent = &alive
		cfg.RequireKVM = false
	}

	svc, err := linuxvmservice.NewService(cfg)
	if err != nil {
		log.Fatalf("service: %v", err)
	}

	ln, err := net.Listen("unix", socketPath)
	if err != nil {
		log.Fatalf("listen: %v", err)
	}
	// SO_PASSCRED so Accept/GetsockoptUcred can read peer pid/uid.
	if ul, ok := ln.(*net.UnixListener); ok {
		raw, err := ul.SyscallConn()
		if err == nil {
			_ = raw.Control(func(fd uintptr) {
				_ = unix.SetsockoptInt(int(fd), unix.SOL_SOCKET, unix.SO_PASSCRED, 1)
			})
		}
	}
	if err := os.Chmod(socketPath, 0o660); err != nil {
		log.Fatalf("chmod socket: %v", err)
	}
	boundSocket, err := socketIdentityAt(socketPath)
	if err != nil {
		log.Fatalf("socket identity: %v", err)
	}
	log.Printf("grok-linux-vm-service listening on %s (peer=SO_PEERCRED+/proc/pid/exe)", socketPath)

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	connectionSlots := make(chan struct{}, maxConnections)
	go func() {
		<-ctx.Done()
		_ = ln.Close()
		_ = removeSocketIfSame(socketPath, boundSocket)
	}()

	for {
		conn, err := ln.Accept()
		if err != nil {
			if ctx.Err() != nil {
				return
			}
			log.Printf("accept: %v", err)
			continue
		}
		select {
		case connectionSlots <- struct{}{}:
			go func() {
				defer func() { <-connectionSlots }()
				handleConn(ctx, conn, svc)
			}()
		default:
			_ = conn.SetWriteDeadline(time.Now().Add(defaultIOTimeout))
			writeErr(conn, "", "busy", "connection limit reached", true)
			_ = conn.Close()
		}
	}
}

func configuredAllowedUID(value string) (uint32, error) {
	trimmed := strings.TrimSpace(value)
	if trimmed == "" {
		if os.Getenv("GROK_LINUX_VM_LAB_SPAWN") == "1" {
			return uint32(os.Geteuid()), nil
		}
		return 0, errors.New("GROK_LINUX_VM_ALLOWED_UID is required outside the lab harness")
	}
	parsed, err := strconv.ParseUint(trimmed, 10, 32)
	if err != nil {
		return 0, errors.New("GROK_LINUX_VM_ALLOWED_UID must be a decimal uint32")
	}
	return uint32(parsed), nil
}

type socketIdentity struct {
	dev uint64
	ino uint64
}

func socketIdentityAt(path string) (socketIdentity, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return socketIdentity{}, err
	}
	if info.Mode()&os.ModeSocket == 0 {
		return socketIdentity{}, errors.New("path is not a socket")
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || stat.Dev == 0 || stat.Ino == 0 {
		return socketIdentity{}, errors.New("socket identity unavailable")
	}
	return socketIdentity{dev: stat.Dev, ino: stat.Ino}, nil
}

func removeSocketIfSame(path string, expected socketIdentity) error {
	actual, err := socketIdentityAt(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if actual != expected {
		return errors.New("refusing to remove identity-swapped socket")
	}
	return os.Remove(path)
}

// prepareSocketPath creates a private service directory and removes only a
// stale socket owned by this service account. An active listener or any other
// filesystem object is never replaced.
func prepareSocketPath(socketPath string) (*os.File, error) {
	if !filepath.IsAbs(socketPath) {
		return nil, errors.New("socket path must be absolute")
	}
	parent := filepath.Dir(socketPath)
	if err := os.MkdirAll(parent, 0o750); err != nil {
		return nil, err
	}
	parentInfo, err := os.Lstat(parent)
	if err != nil {
		return nil, err
	}
	if !parentInfo.IsDir() || parentInfo.Mode()&os.ModeSymlink != 0 || parentInfo.Mode().Perm()&0o027 != 0 {
		return nil, errors.New("socket directory must be a private real directory")
	}
	if stat, ok := parentInfo.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Geteuid() {
		return nil, errors.New("socket directory must be owned by the service account")
	}
	lock, err := acquireSocketLock(socketPath + ".lock")
	if err != nil {
		return nil, err
	}

	info, err := os.Lstat(socketPath)
	if errors.Is(err, os.ErrNotExist) {
		return lock, nil
	}
	if err != nil {
		_ = lock.Close()
		return nil, err
	}
	if info.Mode()&os.ModeSocket == 0 {
		_ = lock.Close()
		return nil, errors.New("refusing to replace a non-socket path")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Geteuid() {
		_ = lock.Close()
		return nil, errors.New("refusing to replace a socket owned by another account")
	}
	probe, dialErr := net.DialTimeout("unix", socketPath, 250*time.Millisecond)
	if dialErr == nil {
		_ = probe.Close()
		_ = lock.Close()
		return nil, errors.New("refusing to replace an active socket")
	}
	if err := os.Remove(socketPath); err != nil {
		_ = lock.Close()
		return nil, err
	}
	return lock, nil
}

func acquireSocketLock(path string) (*os.File, error) {
	fd, err := unix.Open(path, unix.O_RDWR|unix.O_CREAT|unix.O_CLOEXEC|unix.O_NOFOLLOW, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open broker lock: %w", err)
	}
	file := os.NewFile(uintptr(fd), path)
	if file == nil {
		_ = unix.Close(fd)
		return nil, errors.New("broker lock handle unavailable")
	}
	info, err := file.Stat()
	if err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0o077 != 0 {
		_ = file.Close()
		return nil, errors.New("broker lock must be an owner-private regular file")
	}
	if stat, ok := info.Sys().(*syscall.Stat_t); !ok || int(stat.Uid) != os.Geteuid() {
		_ = file.Close()
		return nil, errors.New("broker lock must be owned by the service account")
	}
	if err := unix.Flock(fd, unix.LOCK_EX|unix.LOCK_NB); err != nil {
		_ = file.Close()
		return nil, errors.New("another broker instance owns the socket")
	}
	return file, nil
}

func labRunnerHealthHook() func(string) ([]byte, error) {
	if os.Getenv("GROK_LINUX_VM_LAB_HEALTH") != "ok" {
		return nil
	}
	return func(vmID string) ([]byte, error) {
		return []byte(fmt.Sprintf(`{"status":"ok","vm":%q,"source":"lab-hook"}`, vmID)), nil
	}
}

func labSpawn() linuxvmservice.HypervisorSpawn {
	if os.Getenv("GROK_LINUX_VM_LAB_SPAWN") != "1" {
		return nil // use service default QEMU spawn
	}
	return func(ctx context.Context, vm linuxvmservice.Vm, imageAbsolutePath string) (linuxvmservice.HypervisorProcess, error) {
		if strings.TrimSpace(imageAbsolutePath) == "" {
			return nil, fmt.Errorf("lab spawn requires image path")
		}
		return &labProc{alive: true}, nil
	}
}

func handleConn(ctx context.Context, conn net.Conn, svc *linuxvmservice.Service) {
	defer conn.Close()
	peer, err := peerFromConn(conn)
	if err != nil {
		writeErr(conn, "", "unauthorized", err.Error(), false)
		return
	}
	reader := bufio.NewReader(conn)
	for {
		_ = conn.SetReadDeadline(time.Now().Add(defaultIOTimeout))
		line, err := readBoundedFrame(reader)
		if err != nil {
			if errors.Is(err, errFrameTooLarge) {
				writeErr(conn, "", "protocol", "frame exceeds size limit", false)
			}
			return
		}
		var req requestEnvelope
		decoder := json.NewDecoder(strings.NewReader(string(line)))
		decoder.DisallowUnknownFields()
		if err := decoder.Decode(&req); err != nil || decoder.Decode(&struct{}{}) != io.EOF {
			writeErr(conn, "", "protocol", "invalid json", false)
			return
		}
		if len(line) > maxLegacyFrameBytes && !isScheduledRunEnvelope(req) {
			writeErr(conn, req.ID, "protocol", "frame exceeds operation size limit", false)
			return
		}
		requestCtx, cancel, deadlineErr := requestContext(ctx, req.Deadline)
		if deadlineErr != nil {
			writeErr(conn, req.ID, "deadline", deadlineErr.Error(), false)
			return
		}
		resp := dispatch(requestCtx, svc, peer, req)
		cancel()
		data, _ := json.Marshal(resp)
		data = append(data, '\n')
		if len(data) > maxFrameBytes {
			writeErr(conn, req.ID, "protocol", "response exceeds size limit", false)
			return
		}
		_ = conn.SetWriteDeadline(time.Now().Add(defaultIOTimeout))
		if _, err := conn.Write(data); err != nil {
			return
		}
	}
}

func isScheduledRunEnvelope(req requestEnvelope) bool {
	if req.Operation != "guest_control" {
		return false
	}
	var discriminator struct {
		Method string `json:"method"`
	}
	return json.Unmarshal(req.Payload, &discriminator) == nil && discriminator.Method == "scheduled.run"
}

var errFrameTooLarge = errors.New("frame exceeds size limit")

func readBoundedFrame(reader *bufio.Reader) ([]byte, error) {
	var frame []byte
	for {
		fragment, err := reader.ReadSlice('\n')
		if len(frame)+len(fragment) > maxFrameBytes {
			return nil, errFrameTooLarge
		}
		frame = append(frame, fragment...)
		if err == nil {
			if len(frame) == 1 {
				return nil, errors.New("empty frame")
			}
			return frame, nil
		}
		if !errors.Is(err, bufio.ErrBufferFull) {
			return nil, err
		}
	}
}

func requestContext(parent context.Context, encoded string) (context.Context, context.CancelFunc, error) {
	millis, err := time.ParseDuration(strings.TrimSpace(encoded) + "ms")
	if err != nil || millis < 0 {
		return nil, nil, errors.New("invalid request deadline")
	}
	deadline := time.UnixMilli(millis.Milliseconds())
	now := time.Now()
	if !deadline.After(now) {
		return nil, nil, errors.New("request deadline has expired")
	}
	if deadline.After(now.Add(maxRequestHorizon)) {
		return nil, nil, errors.New("request deadline exceeds maximum horizon")
	}
	ctx, cancel := context.WithDeadline(parent, deadline)
	return ctx, cancel, nil
}

func dispatch(ctx context.Context, svc *linuxvmservice.Service, peer linuxvmservice.PeerIdentity, req requestEnvelope) responseEnvelope {
	if req.Version != wireVersion || req.ID == "" || len(req.ID) > 128 {
		return errResp(req.ID, "protocol", "invalid envelope", false)
	}
	switch req.Operation {
	case "get_capabilities":
		caps, err := svc.GetCapabilities(ctx, peer)
		if err != nil {
			return mapErr(req.ID, err)
		}
		return okResp(req.ID, caps)
	case "ensure_image":
		var payload ensureImagePayload
		if err := decodePayload(req.Payload, &payload); err != nil {
			return errResp(req.ID, "invalid", "invalid ensure_image payload", false)
		}
		img, err := svc.EnsureImage(ctx, peer, payload.ImageID, payload.RelativePath, payload.SHA256, payload.SizeBytes)
		if err != nil {
			return mapErr(req.ID, err)
		}
		return okResp(req.ID, img)
	case "create_vm":
		var payload createVmPayload
		if err := decodePayload(req.Payload, &payload); err != nil {
			return errResp(req.ID, "invalid", "invalid create_vm payload", false)
		}
		if payload.VCPUCount == 0 {
			payload.VCPUCount = 2
		}
		if payload.MemoryMiB == 0 {
			payload.MemoryMiB = 1024
		}
		vm, err := svc.CreateVm(ctx, peer, payload.VmID, payload.ImageID, payload.VCPUCount, payload.MemoryMiB)
		if err != nil {
			// Idempotent product path: treat existing VM as success snapshot via Start later.
			if errors.Is(err, linuxvmservice.ErrInvalid) && strings.Contains(err.Error(), "vm exists") {
				return okResp(req.ID, map[string]string{"id": payload.VmID, "state": "exists"})
			}
			return mapErr(req.ID, err)
		}
		return okResp(req.ID, vm)
	case "start_vm":
		var payload startVmPayload
		if err := decodePayload(req.Payload, &payload); err != nil {
			return errResp(req.ID, "invalid", "invalid start_vm payload", false)
		}
		vm, err := svc.StartVm(ctx, peer, payload.VmID)
		if err != nil {
			return mapErr(req.ID, err)
		}
		return okResp(req.ID, vm)
	case "guest_control":
		var payload guestControlPayload
		if err := decodePayload(req.Payload, &payload); err != nil {
			return errResp(req.ID, "invalid", "invalid guest_control payload", false)
		}
		if payload.Proof != "" {
			if err := svc.GrantGuestControl(ctx, peer, payload.VmID, payload.Proof); err != nil {
				return mapErr(req.ID, err)
			}
		}
		result, err := svc.GuestControl(ctx, peer, linuxvmservice.GuestControlRequest{
			VmID:    payload.VmID,
			Method:  payload.Method,
			Payload: payload.Payload,
		})
		if err != nil {
			return mapErr(req.ID, err)
		}
		return okResp(req.ID, result)
	default:
		return errResp(req.ID, "invalid", "operation not allowlisted on this socket surface", false)
	}
}

func decodePayload(raw json.RawMessage, destination interface{}) error {
	if len(raw) == 0 {
		return errors.New("payload is required")
	}
	decoder := json.NewDecoder(strings.NewReader(string(raw)))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(destination); err != nil {
		return err
	}
	if decoder.Decode(&struct{}{}) != io.EOF {
		return errors.New("payload contains trailing data")
	}
	return nil
}

// peerFromConn resolves peer identity from SO_PEERCRED and /proc/<pid>/exe.
// Client-supplied peerExe is never used for authorization.
func peerFromConn(conn net.Conn) (linuxvmservice.PeerIdentity, error) {
	ucred, err := peerUcred(conn)
	if err != nil {
		return linuxvmservice.PeerIdentity{}, fmt.Errorf("%w: peer credentials: %v", linuxvmservice.ErrUnauthorized, err)
	}
	if ucred.Pid <= 0 {
		return linuxvmservice.PeerIdentity{}, fmt.Errorf("%w: invalid peer pid", linuxvmservice.ErrUnauthorized)
	}
	procExe := fmt.Sprintf("/proc/%d/exe", ucred.Pid)
	handle, err := os.Open(procExe)
	if err != nil {
		return linuxvmservice.PeerIdentity{}, fmt.Errorf("%w: peer exe: %v", linuxvmservice.ErrUnauthorized, err)
	}
	defer handle.Close()
	info, err := handle.Stat()
	if err != nil || !info.Mode().IsRegular() {
		return linuxvmservice.PeerIdentity{}, fmt.Errorf("%w: peer exe identity unavailable", linuxvmservice.ErrUnauthorized)
	}
	identity, ok := info.Sys().(*syscall.Stat_t)
	if !ok || identity.Dev == 0 || identity.Ino == 0 {
		return linuxvmservice.PeerIdentity{}, fmt.Errorf("%w: peer exe identity unavailable", linuxvmservice.ErrUnauthorized)
	}
	exe, err := os.Readlink(procExe)
	if err != nil {
		return linuxvmservice.PeerIdentity{}, fmt.Errorf("%w: peer exe: %v", linuxvmservice.ErrUnauthorized, err)
	}
	exe, err = filepath.EvalSymlinks(exe)
	if err != nil {
		exe = filepath.Clean(exe)
	}
	return linuxvmservice.PeerIdentity{
		UID:            ucred.Uid,
		PID:            ucred.Pid,
		ExecutablePath: exe,
		ExecutableDev:  identity.Dev,
		ExecutableIno:  identity.Ino,
	}, nil
}

func peerUcred(conn net.Conn) (*unix.Ucred, error) {
	unixConn, ok := conn.(*net.UnixConn)
	if !ok {
		return nil, errors.New("not a unix connection")
	}
	raw, err := unixConn.SyscallConn()
	if err != nil {
		return nil, err
	}
	var (
		ucred *unix.Ucred
		opErr error
	)
	err = raw.Control(func(fd uintptr) {
		ucred, opErr = unix.GetsockoptUcred(int(fd), unix.SOL_SOCKET, unix.SO_PEERCRED)
	})
	if err != nil {
		return nil, err
	}
	return ucred, opErr
}

func okResp(id string, result interface{}) responseEnvelope {
	return responseEnvelope{Version: wireVersion, ID: id, OK: true, Result: result}
}

func errResp(id, code, message string, retryable bool) responseEnvelope {
	return responseEnvelope{
		Version: wireVersion,
		ID:      id,
		OK:      false,
		Error:   &wireError{Code: code, Message: message, Retryable: retryable},
	}
}

func mapErr(id string, err error) responseEnvelope {
	switch {
	case errors.Is(err, linuxvmservice.ErrUnauthorized):
		return errResp(id, "unauthorized", err.Error(), false)
	case errors.Is(err, linuxvmservice.ErrUnavailable):
		return errResp(id, "unavailable", err.Error(), true)
	case errors.Is(err, linuxvmservice.ErrNotFound):
		return errResp(id, "not_found", err.Error(), false)
	default:
		return errResp(id, "invalid", err.Error(), false)
	}
}

func writeErr(conn net.Conn, id, code, message string, retryable bool) {
	resp := errResp(id, code, message, retryable)
	data, _ := json.Marshal(resp)
	data = append(data, '\n')
	_, _ = conn.Write(data)
}

func envOr(key, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(key)); value != "" {
		return value
	}
	return fallback
}
