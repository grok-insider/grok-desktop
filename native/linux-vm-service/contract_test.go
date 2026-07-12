package linuxvmservice

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

type fakeProc struct {
	alive bool
	kills int
}

func testPeer(t *testing.T, path string, pid int32) PeerIdentity {
	t.Helper()
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	identity, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		t.Fatal("missing executable identity")
	}
	return PeerIdentity{
		UID:            1000,
		PID:            pid,
		ExecutablePath: path,
		ExecutableDev:  identity.Dev,
		ExecutableIno:  identity.Ino,
	}
}

func (p *fakeProc) Kill() error {
	p.kills++
	p.alive = false
	return nil
}

func (p *fakeProc) Alive() bool { return p.alive }

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
	alive := &fakeProc{alive: true}
	svc, err := NewService(Config{
		ImageRoot:          root,
		AllowedDaemonPaths: []string{daemonPath},
		AllowedDaemonUIDs:  []uint32{1000},
		RequireKVM:         true,
		KVMPresent:         &kvm,
		Spawn: func(ctx context.Context, vm Vm, imageAbsolutePath string) (HypervisorProcess, error) {
			if imageAbsolutePath == "" {
				t.Fatal("spawn requires image path")
			}
			if !alive.alive {
				return nil, fmt.Errorf("dead")
			}
			return alive, nil
		},
		RunnerHealthHook: func(vmID string) ([]byte, error) {
			return []byte(`{"status":"ok","vm":"` + vmID + `"}`), nil
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	peer := testPeer(t, daemonPath, 42)
	ctx := context.Background()

	caps, err := svc.GetCapabilities(ctx, peer)
	if err != nil {
		t.Fatal(err)
	}
	if caps.Simulated {
		t.Fatal("capabilities must not report simulated isolation")
	}
	if caps.Available || caps.Backend != Backend || caps.Reason != "signed_release_evidence_unavailable" {
		t.Fatalf("unsigned lab broker must remain statically unqualified, got %+v", caps)
	}
	if caps.Qualification.BrokerPackageVerified || caps.Qualification.SignedGuestCatalogVerified ||
		caps.Qualification.GuestImageVerified || caps.Qualification.HardwareQualified ||
		caps.Qualification.EvidenceSHA256 != "" {
		t.Fatalf("unsigned broker fabricated qualification evidence: %+v", caps.Qualification)
	}
	for _, operation := range caps.Operations {
		if operation == OperationGuestControl {
			t.Fatal("static capability probe advertised proof-gated guest control")
		}
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

func TestStartVmWithoutSpawnFailsClosed(t *testing.T) {
	root := t.TempDir()
	daemonPath := filepath.Join(root, "grok-daemon")
	_ = os.WriteFile(daemonPath, []byte("x"), 0o755)
	imageBytes := []byte("img")
	rel := "images/guest.raw"
	_ = os.MkdirAll(filepath.Join(root, "images"), 0o700)
	_ = os.WriteFile(filepath.Join(root, rel), imageBytes, 0o600)
	sum := sha256.Sum256(imageBytes)
	kvm := true
	svc, err := NewService(Config{
		ImageRoot:          root,
		AllowedDaemonPaths: []string{daemonPath},
		AllowedDaemonUIDs:  []uint32{1000},
		RequireKVM:         true,
		KVMPresent:         &kvm,
		Spawn: func(ctx context.Context, vm Vm, imageAbsolutePath string) (HypervisorProcess, error) {
			return nil, fmt.Errorf("qemu missing")
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	peer := testPeer(t, daemonPath, 2)
	ctx := context.Background()
	if _, err := svc.EnsureImage(ctx, peer, "guest-v1", rel, hex.EncodeToString(sum[:]), int64(len(imageBytes))); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.CreateVm(ctx, peer, "vm-1", "guest-v1", 2, 1024); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.StartVm(ctx, peer, "vm-1"); err == nil {
		t.Fatal("expected start failure without hypervisor")
	}
}

func TestGuestControlWithoutHealthDialFailsClosed(t *testing.T) {
	root := t.TempDir()
	daemonPath := filepath.Join(root, "grok-daemon")
	_ = os.WriteFile(daemonPath, []byte("x"), 0o755)
	imageBytes := []byte("img2")
	rel := "images/guest.raw"
	_ = os.MkdirAll(filepath.Join(root, "images"), 0o700)
	_ = os.WriteFile(filepath.Join(root, rel), imageBytes, 0o600)
	sum := sha256.Sum256(imageBytes)
	kvm := true
	svc, err := NewService(Config{
		ImageRoot:          root,
		AllowedDaemonPaths: []string{daemonPath},
		AllowedDaemonUIDs:  []uint32{1000},
		RequireKVM:         true,
		KVMPresent:         &kvm,
		Spawn: func(ctx context.Context, vm Vm, imageAbsolutePath string) (HypervisorProcess, error) {
			return &fakeProc{alive: true}, nil
		},
		// No RunnerHealthHook and no GuestHealthDial.
	})
	if err != nil {
		t.Fatal(err)
	}
	peer := testPeer(t, daemonPath, 2)
	ctx := context.Background()
	if _, err := svc.EnsureImage(ctx, peer, "guest-v1", rel, hex.EncodeToString(sum[:]), int64(len(imageBytes))); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.CreateVm(ctx, peer, "vm-1", "guest-v1", 2, 1024); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.StartVm(ctx, peer, "vm-1"); err != nil {
		t.Fatal(err)
	}
	if err := svc.GrantGuestControl(ctx, peer, "vm-1", "proof-of-possession-token-32b-min"); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.GuestControl(ctx, peer, GuestControlRequest{VmID: "vm-1", Method: "runner.health"}); err == nil {
		t.Fatal("expected guest control failure without dial")
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
		AllowedDaemonUIDs:  []uint32{1000},
		RequireKVM:         true,
		KVMPresent:         &kvm,
	})
	if err != nil {
		t.Fatal(err)
	}
	peer := testPeer(t, daemonPath, 2)
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

func TestAuthorizePeerRejectsReplacedAllowlistedExecutable(t *testing.T) {
	root := t.TempDir()
	daemonPath := filepath.Join(root, "grok-daemon")
	if err := os.WriteFile(daemonPath, []byte("first"), 0o755); err != nil {
		t.Fatal(err)
	}
	svc, err := NewService(Config{
		ImageRoot:          root,
		AllowedDaemonPaths: []string{daemonPath},
		AllowedDaemonUIDs:  []uint32{1000},
		RequireKVM:         false,
	})
	if err != nil {
		t.Fatal(err)
	}
	original := testPeer(t, daemonPath, 42)
	if err := svc.AuthorizePeer(original); err != nil {
		t.Fatalf("original identity: %v", err)
	}
	wrongUID := original
	wrongUID.UID = 1001
	if err := svc.AuthorizePeer(wrongUID); err == nil {
		t.Fatal("wrong SO_PEERCRED uid was accepted")
	}
	replacementPath := filepath.Join(root, "replacement")
	if err := os.WriteFile(replacementPath, []byte("replacement"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(replacementPath, daemonPath); err != nil {
		t.Fatal(err)
	}
	replacement := testPeer(t, daemonPath, 43)
	if err := svc.AuthorizePeer(replacement); err == nil {
		t.Fatal("replacement executable identity was accepted")
	}
}

func TestScheduledRunRequiresQualificationLiveGuestAndPoP(t *testing.T) {
	root := t.TempDir()
	daemonPath := filepath.Join(root, "grok-daemon")
	_ = os.WriteFile(daemonPath, []byte("daemon"), 0o755)
	image := []byte("image")
	_ = os.MkdirAll(filepath.Join(root, "images"), 0o700)
	_ = os.WriteFile(filepath.Join(root, "images/guest.raw"), image, 0o600)
	digest := sha256.Sum256(image)
	kvm := true
	var observed ScheduledRunRequest
	var observedDeadline bool
	svc, err := NewService(Config{
		ImageRoot:          root,
		AllowedDaemonPaths: []string{daemonPath},
		AllowedDaemonUIDs:  []uint32{1000},
		KVMPresent:         &kvm,
		Spawn: func(context.Context, Vm, string) (HypervisorProcess, error) {
			return &fakeProc{alive: true}, nil
		},
		ScheduledRunDial: func(ctx context.Context, _ string, _ string, request ScheduledRunRequest) (ScheduledRunOutcome, error) {
			observed = request
			_, observedDeadline = ctx.Deadline()
			return ScheduledRunSucceeded, nil
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	peer := testPeer(t, daemonPath, 42)
	ctx := context.Background()
	if _, err := svc.EnsureImage(ctx, peer, "guest-v1", "images/guest.raw", hex.EncodeToString(digest[:]), int64(len(image))); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.CreateVm(ctx, peer, "work-vm", "guest-v1", 2, 1024); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.StartVm(ctx, peer, "work-vm"); err != nil {
		t.Fatal(err)
	}
	payload := scheduledPayload("occurrence-1", "run-1", []byte("prompt"))
	request := GuestControlRequest{VmID: "work-vm", Method: "scheduled.run", Payload: payload}
	if _, err := svc.GuestControl(ctx, peer, request); err == nil {
		t.Fatal("scheduled run crossed without PoP")
	}
	if err := svc.GrantGuestControl(ctx, peer, "work-vm", "proof-of-possession-token-32b-min"); err != nil {
		t.Fatal(err)
	}
	if _, err := svc.GuestControl(ctx, peer, request); err == nil {
		t.Fatal("unqualified broker exposed scheduled.run")
	}
	svc.qualification = &QualificationEvidence{
		BrokerPackageVerified: true, SignedGuestCatalogVerified: true,
		GuestImageVerified: true, HardwareQualified: true, EvidenceSHA256: strings.Repeat("a", 64),
	}
	scheduledCtx, cancel := context.WithTimeout(ctx, time.Second)
	defer cancel()
	response, err := svc.GuestControl(scheduledCtx, peer, request)
	if err != nil {
		t.Fatal(err)
	}
	if response.Method != "scheduled.run" || response.Outcome != "succeeded" || observed.Prompt != "prompt" ||
		observed.OccurrenceID != "occurrence-1" || observed.RunID != "run-1" || !observedDeadline {
		t.Fatalf("unexpected scheduled result=%+v request=%+v", response, observed)
	}
	svc.config.ScheduledRunDial = func(context.Context, string, string, ScheduledRunRequest) (ScheduledRunOutcome, error) {
		return ScheduledRunOutcome("unknown"), nil
	}
	if _, err := svc.GuestControl(ctx, peer, request); err == nil {
		t.Fatal("unknown scheduled outcome was accepted")
	}
}

func TestScheduledPayloadBoundsAndClosedFields(t *testing.T) {
	maximum := scheduledPayload("occurrence", "run", bytes.Repeat([]byte("x"), maxScheduledPromptBytes))
	if _, err := decodeScheduledRunPayload(maximum); err != nil {
		t.Fatalf("maximum prompt rejected: %v", err)
	}
	tooLarge := scheduledPayload("occurrence", "run", bytes.Repeat([]byte("x"), maxScheduledPromptBytes+1))
	if _, err := decodeScheduledRunPayload(tooLarge); err == nil {
		t.Fatal("oversized prompt accepted")
	}
	for _, malformed := range [][]byte{
		append(maximum, 0),
		scheduledPayload("bad/id", "run", []byte("prompt")),
		scheduledPayload("occurrence", "run", []byte{'x', 0}),
	} {
		if _, err := decodeScheduledRunPayload(malformed); err == nil {
			t.Fatal("malformed scheduled payload accepted")
		}
	}
}

func scheduledPayload(occurrenceID, runID string, prompt []byte) []byte {
	payload := append([]byte(nil), scheduledPayloadMagic...)
	var short [2]byte
	binary.BigEndian.PutUint16(short[:], uint16(len(occurrenceID)))
	payload = append(payload, short[:]...)
	payload = append(payload, occurrenceID...)
	binary.BigEndian.PutUint16(short[:], uint16(len(runID)))
	payload = append(payload, short[:]...)
	payload = append(payload, runID...)
	var length [4]byte
	binary.BigEndian.PutUint32(length[:], uint32(len(prompt)))
	payload = append(payload, length[:]...)
	return append(payload, prompt...)
}
