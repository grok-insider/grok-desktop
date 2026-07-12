package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestReadBoundedFrame(t *testing.T) {
	valid := []byte(`{"version":"1.0.0"}` + "\n")
	frame, err := readBoundedFrame(bufio.NewReader(strings.NewReader(string(valid))))
	if err != nil || string(frame) != string(valid) {
		t.Fatalf("valid frame: %q, %v", frame, err)
	}
	oversized := strings.Repeat("x", maxFrameBytes) + "\n"
	if _, err := readBoundedFrame(bufio.NewReader(strings.NewReader(oversized))); !errors.Is(err, errFrameTooLarge) {
		t.Fatalf("oversized frame was not rejected: %v", err)
	}
	if _, err := readBoundedFrame(bufio.NewReader(strings.NewReader("unterminated"))); err == nil {
		t.Fatal("unterminated frame was accepted")
	}
}

func TestOnlyScheduledRunReceivesExpandedFrameBudget(t *testing.T) {
	scheduled := requestEnvelope{
		Operation: "guest_control",
		Payload:   json.RawMessage(`{"vmId":"work-vm","method":"scheduled.run","proof":"proof","payload":"AA=="}`),
	}
	if !isScheduledRunEnvelope(scheduled) {
		t.Fatal("closed scheduled.run envelope was not recognized")
	}
	for _, request := range []requestEnvelope{
		{Operation: "ensure_image", Payload: json.RawMessage(`{"method":"scheduled.run"}`)},
		{Operation: "guest_control", Payload: json.RawMessage(`{"method":"runner.health"}`)},
	} {
		if isScheduledRunEnvelope(request) {
			t.Fatal("non-scheduled operation received expanded frame budget")
		}
	}
	exact := strings.Repeat("x", maxFrameBytes-1) + "\n"
	frame, err := readBoundedFrame(bufio.NewReader(strings.NewReader(exact)))
	if err != nil || len(frame) != maxFrameBytes {
		t.Fatalf("exact maximum frame: %d, %v", len(frame), err)
	}
}

func TestDecodePayloadRejectsUnknownAndTrailingFields(t *testing.T) {
	for _, raw := range []string{
		`{"vmId":"vm-1","unknown":true}`,
		`{"vmId":"vm-1"} {}`,
		``,
	} {
		var payload startVmPayload
		if err := decodePayload(json.RawMessage(raw), &payload); err == nil {
			t.Fatalf("payload %q was accepted", raw)
		}
	}
	var payload startVmPayload
	if err := decodePayload(json.RawMessage(`{"vmId":"vm-1"}`), &payload); err != nil {
		t.Fatalf("valid payload: %v", err)
	}
}

func TestConfiguredAllowedUIDRequiresExplicitProductionValue(t *testing.T) {
	t.Setenv("GROK_LINUX_VM_LAB_SPAWN", "0")
	if _, err := configuredAllowedUID(""); err == nil {
		t.Fatal("missing production uid was accepted")
	}
	if _, err := configuredAllowedUID("-1"); err == nil {
		t.Fatal("negative uid was accepted")
	}
	uid, err := configuredAllowedUID("1000")
	if err != nil || uid != 1000 {
		t.Fatalf("explicit uid: %d, %v", uid, err)
	}
}

func TestRequestContextRejectsExpiredMalformedAndUnboundedDeadlines(t *testing.T) {
	now := time.Now()
	for _, value := range []string{
		"not-a-number",
		"-1",
		formatUnixMillis(now.Add(-time.Second)),
		formatUnixMillis(now.Add(maxRequestHorizon + time.Second)),
	} {
		if _, _, err := requestContext(context.Background(), value); err == nil {
			t.Fatalf("deadline %q was accepted", value)
		}
	}
	ctx, cancel, err := requestContext(context.Background(), formatUnixMillis(now.Add(time.Second)))
	if err != nil {
		t.Fatalf("bounded deadline: %v", err)
	}
	defer cancel()
	if _, ok := ctx.Deadline(); !ok {
		t.Fatal("request context has no deadline")
	}
}

func TestPrepareSocketPathRejectsObjectsAndActiveListenerAndRecoversStaleSocket(t *testing.T) {
	root := t.TempDir()
	if err := os.Chmod(root, 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "broker.sock")
	if err := os.WriteFile(path, []byte("do not replace"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := prepareSocketPath(path); err == nil {
		t.Fatal("regular file was accepted")
	}
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	listener, err := net.Listen("unix", path)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := prepareSocketPath(path); err == nil {
		t.Fatal("active listener was accepted as stale")
	}
	if err := listener.Close(); err != nil {
		t.Fatal(err)
	}
	lock, err := prepareSocketPath(path)
	if err != nil {
		t.Fatalf("stale owned socket was not recovered: %v", err)
	}
	defer lock.Close()
	if _, err := os.Lstat(path); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("stale socket still exists: %v", err)
	}
}

func TestPrepareSocketPathRequiresPrivateOwnedDirectory(t *testing.T) {
	root := t.TempDir()
	public := filepath.Join(root, "public")
	if err := os.Mkdir(public, 0o777); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(public, 0o777); err != nil {
		t.Fatal(err)
	}
	if _, err := prepareSocketPath(filepath.Join(public, "broker.sock")); err == nil {
		t.Fatal("world-writable socket directory was accepted")
	}
}

func TestSocketCleanupRejectsIdentitySwap(t *testing.T) {
	root := t.TempDir()
	if err := os.Chmod(root, 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "broker.sock")
	first, err := net.Listen("unix", path)
	if err != nil {
		t.Fatal(err)
	}
	firstIdentity, err := socketIdentityAt(path)
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close()
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	second, err := net.Listen("unix", path)
	if err != nil {
		t.Fatal(err)
	}
	defer second.Close()
	if err := removeSocketIfSame(path, firstIdentity); err == nil {
		t.Fatal("identity-swapped socket was removed")
	}
	if _, err := os.Lstat(path); err != nil {
		t.Fatalf("replacement socket disappeared: %v", err)
	}
}

func TestSocketLockRejectsConcurrentBrokerOwnership(t *testing.T) {
	root := t.TempDir()
	if err := os.Chmod(root, 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "broker.sock")
	first, err := prepareSocketPath(path)
	if err != nil {
		t.Fatal(err)
	}
	defer first.Close()
	if _, err := prepareSocketPath(path); err == nil {
		t.Fatal("concurrent broker acquired the socket lock")
	}
}

func formatUnixMillis(value time.Time) string {
	return fmt.Sprintf("%d", value.UnixMilli())
}
