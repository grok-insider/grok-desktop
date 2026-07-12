package linuxvmservice

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"os"
	"path/filepath"
	"testing"
)

func TestLifecycleAndGuestControlGates(t *testing.T) {
	root := t.TempDir()
	daemonPath := filepath.Join(root, "grok-daemon")
	if err := os.WriteFile(daemonPath, []byte("#!/bin/sh\n"), 0o755); err != nil {
		t.Fatal(err)
	}
	imageBytes := []byte("guest-image-payload-for-digest")
	rel := "images/guest.raw"
	if err := os.MkdirAll(filepath.Join(root, "images"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, rel), imageBytes, 0o600); err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(imageBytes)
	digest := hex.EncodeToString(sum[:])

	kvm := true
	svc, err := NewService(Config{
		ImageRoot:          root,
		AllowedDaemonPaths: []string{daemonPath},
		RequireKVM:         true,
		KVMPresent:         &kvm,
		RunnerHealthHook: func(vmID string) ([]byte, error) {
			return []byte(`{"status":"ok","vm":"` + vmID + `"}`), nil
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	peer := PeerIdentity{UID: 1000, PID: 42, ExecutablePath: daemonPath}
	ctx := context.Background()

	caps, err := svc.GetCapabilities(ctx, peer)
	if err != nil {
		t.Fatal(err)
	}
	if caps.Simulated {
		t.Fatal("capabilities must not report simulated isolation")
	}
	if !caps.Available || caps.Backend != Backend {
		t.Fatalf("expected available qemu-kvm, got %+v", caps)
	}

	badPeer := PeerIdentity{UID: 1000, PID: 1, ExecutablePath: "/usr/bin/evil"}
	if _, err := svc.GetCapabilities(ctx, badPeer); err == nil {
		t.Fatal("expected unauthorized peer rejection")
	}

	img, err := svc.EnsureImage(ctx, peer, "guest-v1", rel, digest, int64(len(imageBytes)))
	if err != nil {
		t.Fatal(err)
	}
	if img.SHA256 != digest {
		t.Fatalf("digest %s", img.SHA256)
	}
	if _, err := svc.EnsureImage(ctx, peer, "guest-v1", rel, digest, int64(len(imageBytes))+1); err == nil {
		t.Fatal("expected size mismatch")
	}

	vm, err := svc.CreateVm(ctx, peer, "vm-1", "guest-v1", 2, 1024)
	if err != nil {
		t.Fatal(err)
	}
	if vm.State != VmStateCreated {
		t.Fatalf("state %s", vm.State)
	}

	// Guest control before grant and before start must fail.
	if _, err := svc.GuestControl(ctx, peer, GuestControlRequest{VmID: "vm-1", Method: "runner.health"}); err == nil {
		t.Fatal("expected grant failure")
	}

	started, err := svc.StartVm(ctx, peer, "vm-1")
	if err != nil {
		t.Fatal(err)
	}
	if started.State != VmStateRunning || started.BootID == "" {
		t.Fatalf("started %+v", started)
	}
	if err := svc.GrantGuestControl(ctx, peer, "vm-1", "proof-of-possession-token-32b-min"); err != nil {
		t.Fatal(err)
	}
	resp, err := svc.GuestControl(ctx, peer, GuestControlRequest{VmID: "vm-1", Method: "runner.health"})
	if err != nil {
		t.Fatal(err)
	}
	if string(resp.Body) == "" || resp.Method != "runner.health" {
		t.Fatalf("response %+v", resp)
	}
	if _, err := svc.GuestControl(ctx, peer, GuestControlRequest{VmID: "vm-1", Method: "shell.exec"}); err == nil {
		t.Fatal("expected method reject")
	}

	// Attach while running forbidden.
	if _, err := svc.AttachWorkspace(ctx, peer, "vm-1", "ws/a", "/run/grok-desktop/workspaces/a"); err == nil {
		t.Fatal("expected attach while running fail")
	}
	if _, err := svc.StopVm(ctx, peer, "vm-1"); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.AttachWorkspace(ctx, peer, "vm-1", "ws/a", "/run/grok-desktop/workspaces/a"); err != nil {
		t.Fatal(err)
	}
	if err := svc.DeleteVm(ctx, peer, "vm-1"); err != nil {
		t.Fatal(err)
	}
}

func TestKvmUnavailableFailClosed(t *testing.T) {
	root := t.TempDir()
	daemonPath := filepath.Join(root, "grok-daemon")
	_ = os.WriteFile(daemonPath, []byte("x"), 0o755)
	kvm := false
	svc, err := NewService(Config{
		ImageRoot:          root,
		AllowedDaemonPaths: []string{daemonPath},
		RequireKVM:         true,
		KVMPresent:         &kvm,
	})
	if err != nil {
		t.Fatal(err)
	}
	peer := PeerIdentity{UID: 1, PID: 2, ExecutablePath: daemonPath}
	caps, err := svc.GetCapabilities(context.Background(), peer)
	if err != nil {
		t.Fatal(err)
	}
	if caps.Available || caps.Simulated {
		t.Fatalf("expected unavailable non-simulated, got %+v", caps)
	}
	if _, err := svc.EnsureImage(context.Background(), peer, "a", "x", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 1); err == nil {
		t.Fatal("ensure must fail without kvm")
	}
}
