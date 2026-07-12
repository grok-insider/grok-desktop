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
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"syscall"
	"time"

	linuxvmservice "github.com/grok-insider/grok-desktop/native/linux-vm-service"
	"golang.org/x/sys/unix"
)

type requestEnvelope struct {
	Version   string          `json:"version"`
	ID        string          `json:"id"`
	Operation string          `json:"operation"`
	Deadline  string          `json:"deadline"`
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
	socketPath := envOr("GROK_LINUX_VM_SOCKET", filepath.Join(os.TempDir(), "grok-linux-vm-service.sock"))
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
	if err := os.MkdirAll(imageRoot, 0o700); err != nil {
		log.Fatalf("image root: %v", err)
	}
	_ = os.Remove(socketPath)

	requireKVM := os.Getenv("GROK_LINUX_VM_REQUIRE_KVM") != "0"
	cfg := linuxvmservice.Config{
		ImageRoot:          imageRoot,
		AllowedDaemonPaths: []string{daemonPath},
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
	log.Printf("grok-linux-vm-service listening on %s (peer=SO_PEERCRED+/proc/pid/exe)", socketPath)

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	go func() {
		<-ctx.Done()
		_ = ln.Close()
		_ = os.Remove(socketPath)
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
		go handleConn(ctx, conn, svc)
	}
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
		_ = conn.SetReadDeadline(time.Now().Add(120 * time.Second))
		line, err := reader.ReadBytes('\n')
		if err != nil {
			return
		}
		var req requestEnvelope
		if err := json.Unmarshal(line, &req); err != nil {
			writeErr(conn, "", "protocol", "invalid json", false)
			return
		}
		resp := dispatch(ctx, svc, peer, req)
		data, _ := json.Marshal(resp)
		data = append(data, '\n')
		if _, err := conn.Write(data); err != nil {
			return
		}
	}
}

func dispatch(ctx context.Context, svc *linuxvmservice.Service, peer linuxvmservice.PeerIdentity, req requestEnvelope) responseEnvelope {
	if req.Version != "1.0.0" || req.ID == "" {
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
		if err := json.Unmarshal(req.Payload, &payload); err != nil {
			return errResp(req.ID, "invalid", "invalid ensure_image payload", false)
		}
		img, err := svc.EnsureImage(ctx, peer, payload.ImageID, payload.RelativePath, payload.SHA256, payload.SizeBytes)
		if err != nil {
			return mapErr(req.ID, err)
		}
		return okResp(req.ID, img)
	case "create_vm":
		var payload createVmPayload
		if err := json.Unmarshal(req.Payload, &payload); err != nil {
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
		if err := json.Unmarshal(req.Payload, &payload); err != nil {
			return errResp(req.ID, "invalid", "invalid start_vm payload", false)
		}
		vm, err := svc.StartVm(ctx, peer, payload.VmID)
		if err != nil {
			return mapErr(req.ID, err)
		}
		return okResp(req.ID, vm)
	case "guest_control":
		var payload guestControlPayload
		if err := json.Unmarshal(req.Payload, &payload); err != nil {
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
	exe, err := os.Readlink(fmt.Sprintf("/proc/%d/exe", ucred.Pid))
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
	return responseEnvelope{Version: "1.0.0", ID: id, OK: true, Result: result}
}

func errResp(id, code, message string, retryable bool) responseEnvelope {
	return responseEnvelope{
		Version: "1.0.0",
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
