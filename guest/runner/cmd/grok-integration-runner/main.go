package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"net"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/grok-insider/grok-desktop/guest/runner/internal/runner"
	"github.com/grok-insider/grok-desktop/guest/runner/internal/workspace"
)

func main() {
	policyPath := flag.String("policy", "/etc/grok-desktop/policy.json", "absolute runner policy path")
	flag.Parse()
	if flag.NArg() != 0 || *policyPath == "" {
		fatal("invalid command line")
	}
	policy, trust, err := runner.LoadPolicy(*policyPath)
	if err != nil {
		fatal("runner policy could not be loaded")
	}
	manager, err := runner.NewManager(policy, trust)
	if err != nil {
		fatal("integration manager could not be initialized")
	}
	mounter, err := workspace.NewClient(policy.WorkspaceMounterSocket, policy.WorkspaceRoot)
	if err != nil {
		fatal("workspace mounter client could not be initialized")
	}
	server, err := runner.NewControlServer(policy, manager, mounter)
	if err != nil {
		fatal("control server could not be initialized")
	}
	listener, err := runner.ListenHostVSock(policy.ControlPort)
	if err != nil {
		fatal("control listener could not be initialized")
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if err := server.Serve(ctx, listener, func() error {
		return notifySystemd("READY=1\nSTATUS=Authenticated guest channel ready")
	}); err != nil {
		fatal("control server stopped unexpectedly")
	}
	shutdown, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	if err := manager.Close(shutdown); err != nil && !errors.Is(err, context.Canceled) {
		fatal("integration manager shutdown timed out")
	}
	_ = notifySystemd("STOPPING=1\nSTATUS=Stopped")
}

func notifySystemd(state string) error {
	address := os.Getenv("NOTIFY_SOCKET")
	if address == "" {
		return nil
	}
	if strings.HasPrefix(address, "@") {
		address = "\x00" + strings.TrimPrefix(address, "@")
	}
	connection, err := net.DialUnix("unixgram", nil, &net.UnixAddr{Name: address, Net: "unixgram"})
	if err != nil {
		return err
	}
	defer connection.Close()
	_, err = connection.Write([]byte(state))
	return err
}

func fatal(message string) {
	_, _ = fmt.Fprintln(os.Stderr, message)
	os.Exit(1)
}
