// Command grok-linux-vm-service is the privileged Linux isolation broker entry
// point (platform ADR 0004). It speaks a bounded JSON-lines protocol over a
// unix domain socket and never returns raw guest endpoints.
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
)

type requestEnvelope struct {
	Version   string          `json:"version"`
	ID        string          `json:"id"`
	Operation string          `json:"operation"`
	Deadline  string          `json:"deadline"`
	PeerExe   string          `json:"peerExe,omitempty"`
	Payload   json.RawMessage `json:"payload"`
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

func main() {
	socketPath := envOr("GROK_LINUX_VM_SOCKET", filepath.Join(os.TempDir(), "grok-linux-vm-service.sock"))
	imageRoot := envOr("GROK_LINUX_VM_IMAGE_ROOT", filepath.Join(os.TempDir(), "grok-linux-vm-images"))
	daemonPath := envOr("GROK_LINUX_VM_ALLOWED_DAEMON", "")
	if daemonPath == "" {
		// Development default: accept any path under cwd for the unit smoke path.
		// Production packaging must set GROK_LINUX_VM_ALLOWED_DAEMON to the exact grok-daemon path.
		cwd, _ := os.Getwd()
		daemonPath = filepath.Join(cwd, "target", "debug", "grok-daemon")
	}
	if err := os.MkdirAll(imageRoot, 0o700); err != nil {
		log.Fatalf("image root: %v", err)
	}
	_ = os.Remove(socketPath)

	svc, err := linuxvmservice.NewService(linuxvmservice.Config{
		ImageRoot:          imageRoot,
		AllowedDaemonPaths: []string{daemonPath},
		RequireKVM:         true,
		// Lab dial for runner.health when a harness injects a live process + hook via env.
		RunnerHealthHook: labRunnerHealthHook(),
	})
	if err != nil {
		log.Fatalf("service: %v", err)
	}

	ln, err := net.Listen("unix", socketPath)
	if err != nil {
		log.Fatalf("listen: %v", err)
	}
	if err := os.Chmod(socketPath, 0o660); err != nil {
		log.Fatalf("chmod socket: %v", err)
	}
	log.Printf("grok-linux-vm-service listening on %s", socketPath)

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

func handleConn(ctx context.Context, conn net.Conn, svc *linuxvmservice.Service) {
	defer conn.Close()
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
		peer := peerFromRequest(req)
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

func peerFromRequest(req requestEnvelope) linuxvmservice.PeerIdentity {
	// Development socket: identity is validated against AllowedDaemonPaths using
	// the peerExe field the daemon sends. Production should migrate to
	// SCM_CREDENTIALS so the broker never trusts a client-supplied path alone.
	exe := strings.TrimSpace(req.PeerExe)
	if exe == "" {
		exe = envOr("GROK_LINUX_VM_ALLOWED_DAEMON", "")
	}
	return linuxvmservice.PeerIdentity{UID: uint32(os.Getuid()), PID: int32(os.Getpid()), ExecutablePath: exe}
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
