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

	"github.com/grok-insider/grok-desktop/guest/runner/internal/runner"
	"github.com/grok-insider/grok-desktop/guest/runner/internal/workspace"
)

func main() {
	policyPath := flag.String("policy", "/etc/grok-desktop/policy.json", "absolute runner policy path")
	runnerUser := flag.String("runner-user", "grok-integrations", "unprivileged runner user")
	runnerGroup := flag.String("runner-group", "grok-integrations", "unprivileged runner group")
	flag.Parse()
	if flag.NArg() != 0 || *policyPath == "" || *runnerUser == "" || *runnerGroup == "" {
		fatal("invalid command line")
	}
	policy, _, err := runner.LoadPolicy(*policyPath)
	if err != nil {
		fatal("workspace mounter policy could not be loaded")
	}
	broker, err := workspace.NewBroker(workspace.BrokerConfig{
		SocketPath: policy.WorkspaceMounterSocket, WorkspaceRoot: policy.WorkspaceRoot,
		RunnerUser: *runnerUser, RunnerGroup: *runnerGroup,
	})
	if err != nil {
		fatal("workspace mounter could not be initialized")
	}
	defer broker.Close()
	if err := broker.Listen(); err != nil {
		fatal("workspace mounter listener could not be initialized")
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if err := notifySystemd("READY=1\nSTATUS=Workspace mount broker ready"); err != nil {
		fatal("service readiness could not be reported")
	}
	if err := broker.Serve(ctx); err != nil && !errors.Is(err, context.Canceled) {
		fatal("workspace mounter stopped unexpectedly")
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
