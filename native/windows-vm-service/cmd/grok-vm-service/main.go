package main

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"os"
	"os/signal"
	"runtime"
	"time"

	vmservice "github.com/grok-insider/grok-desktop/native/windows-vm-service"
	"github.com/grok-insider/grok-desktop/native/windows-vm-service/host"
	"github.com/grok-insider/grok-desktop/native/windows-vm-service/tenant"
	"github.com/grok-insider/grok-desktop/native/windows-vm-service/transport"
)

const serviceName = "GrokDesktopVmBroker"

const guestCatalogTrustBindingPrefix = "grok-guest-catalog-trust-v1:"

var (
	version                  = "development"
	guestCatalogTrust        = ""
	guestCatalogTrustBinding = ""
)

type options struct {
	dataRoot           string
	releaseRoot        string
	guestCatalogTrust  string
	developmentUserSID string
	endpoint           string
	maxTenants         int
	maxMessageBytes    int
	maxRequestDeadline time.Duration
	shutdownTimeout    time.Duration
	foreground         bool
	showVersion        bool
}

func main() {
	logger := slog.New(slog.NewJSONHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelInfo}))
	if err := run(os.Args[1:], os.Stdout, logger); err != nil {
		// Startup errors can wrap operating-system paths or identities. Detailed
		// diagnostics belong in protected qualification traces, not service logs.
		logger.Error("VM service stopped with an error")
		os.Exit(1)
	}
}

func run(arguments []string, stdout io.Writer, logger *slog.Logger) error {
	options, err := parseOptions(arguments)
	if err != nil {
		return err
	}
	if options.showVersion {
		_, err := fmt.Fprintln(stdout, version)
		return err
	}
	if err := finalizePlatformOptions(&options); err != nil {
		return err
	}
	serviceMode, err := isServiceProcess()
	if err != nil {
		return fmt.Errorf("detect Windows service context: %w", err)
	}
	if serviceMode {
		if options.foreground {
			return fmt.Errorf("--foreground cannot be used under the Service Control Manager")
		}
		return runService(options, logger)
	}
	if explicitForegroundRequired() && !options.foreground {
		return fmt.Errorf("interactive Windows starts require --foreground")
	}
	return runForeground(options, logger)
}

func parseOptions(arguments []string) (options, error) {
	flags := flag.NewFlagSet("grok-vm-service", flag.ContinueOnError)
	flags.SetOutput(io.Discard)
	var result options
	flags.StringVar(&result.dataRoot, "data-root", "", "absolute service-owned data root")
	flags.StringVar(&result.releaseRoot, "release-root", "", "packaged release root for non-Windows development only")
	flags.StringVar(&result.developmentUserSID, "development-user-sid", "", "simulated SID for non-Windows development only")
	flags.StringVar(&result.endpoint, "listen", "", "non-Windows development endpoint; production uses a fixed pipe")
	flags.IntVar(&result.maxTenants, "max-tenants", tenant.DefaultMaxTenants, "maximum active tenant backends")
	flags.IntVar(&result.maxMessageBytes, "max-message-bytes", host.DefaultMaxMessageBytes, "maximum JSON Lines frame size")
	flags.DurationVar(&result.maxRequestDeadline, "max-request-deadline", host.DefaultMaxRequestDeadline, "maximum client deadline horizon")
	flags.DurationVar(&result.shutdownTimeout, "shutdown-timeout", host.DefaultShutdownTimeout, "graceful shutdown window")
	flags.BoolVar(&result.foreground, "foreground", false, "run as an interactive diagnostic process")
	flags.BoolVar(&result.showVersion, "version", false, "print version and exit")
	if err := flags.Parse(arguments); err != nil {
		return options{}, fmt.Errorf("parse command line: %w", err)
	}
	if flags.NArg() != 0 {
		return options{}, fmt.Errorf("unexpected positional arguments")
	}
	return result, nil
}

func runForeground(options options, logger *slog.Logger) error {
	ctx, stop := signal.NotifyContext(context.Background(), shutdownSignals()...)
	defer stop()
	return runHost(ctx, options, logger)
}

func runHost(ctx context.Context, options options, logger *slog.Logger) error {
	architecture, err := vmservice.GuestArchitectureForRuntime(runtime.GOARCH)
	if err != nil {
		return fmt.Errorf("resolve guest image architecture: %w", err)
	}
	trust, err := vmservice.ParseGuestImageTrust(options.guestCatalogTrust)
	if err != nil {
		return fmt.Errorf("initialize guest image trust: %w", err)
	}
	policy, err := vmservice.LoadGuestImagePolicy(options.releaseRoot, architecture, trust)
	if err != nil {
		return fmt.Errorf("load official guest image policy: %w", err)
	}
	if err := tenant.PrepareServiceStorage(options.dataRoot); err != nil {
		return fmt.Errorf("prepare service storage: %w", err)
	}
	if err := vmservice.EnforceGuestImagePolicyRollback(options.dataRoot, policy); err != nil {
		return fmt.Errorf("enforce guest image catalog rollback policy: %w", err)
	}
	guestControlMaxBytes := options.maxMessageBytes - (64 << 10)
	if guestControlMaxBytes < 4096 {
		return fmt.Errorf("--max-message-bytes must leave at least 4096 bytes for guest control after envelope overhead")
	}
	if guestControlMaxBytes > vmservice.DefaultGuestControlMaxBytes {
		guestControlMaxBytes = vmservice.DefaultGuestControlMaxBytes
	}
	manager, err := tenant.NewManager(tenant.Config{
		DataRoot: options.dataRoot, MaxTenants: options.maxTenants,
		GuestImagePolicy:           policy,
		GuestControlMaxBytes:       guestControlMaxBytes,
		AllowDevelopmentIdentities: allowDevelopmentIdentities(),
		Factory:                    tenant.BackendFactoryFunc(vmservice.NewPlatformServiceContext),
	})
	if err != nil {
		return fmt.Errorf("initialize tenant manager: %w", err)
	}
	defer func() {
		closeContext, cancel := context.WithTimeout(context.Background(), options.shutdownTimeout)
		defer cancel()
		if closeErr := manager.Close(closeContext); closeErr != nil {
			logger.Error("tenant manager did not stop within its deadline")
		}
	}()

	server, err := host.New(host.Config{
		Resolver:           manager,
		Logger:             logger,
		MaxMessageBytes:    options.maxMessageBytes,
		MaxRequestDeadline: options.maxRequestDeadline,
		ShutdownTimeout:    options.shutdownTimeout,
	})
	if err != nil {
		return fmt.Errorf("initialize service host: %w", err)
	}
	listener, err := transport.Listen(transport.Config{
		Endpoint: options.endpoint, DevelopmentPeerSID: options.developmentUserSID,
		MaxMessageBytes: options.maxMessageBytes,
	})
	if err != nil {
		return fmt.Errorf("initialize authenticated transport: %w", err)
	}

	logger.Info("VM service listening", "version", version)
	if err := server.Serve(ctx, listener); err != nil {
		return err
	}
	logger.Info("VM service stopped")
	return nil
}

func decodeCompiledGuestCatalogTrust() (string, error) {
	if guestCatalogTrust == "" && guestCatalogTrustBinding == "" {
		return "", nil
	}
	if guestCatalogTrust == "" || guestCatalogTrustBinding == "" {
		return "", fmt.Errorf("compiled guest image catalog trust is incomplete")
	}
	decoded, err := base64.StdEncoding.Strict().DecodeString(guestCatalogTrust)
	if err != nil || len(decoded) == 0 || base64.StdEncoding.EncodeToString(decoded) != guestCatalogTrust {
		return "", fmt.Errorf("compiled guest image catalog trust is malformed")
	}
	digest := sha256.Sum256([]byte(guestCatalogTrust))
	expectedBinding := guestCatalogTrustBindingPrefix + hex.EncodeToString(digest[:])
	if guestCatalogTrustBinding != expectedBinding {
		return "", fmt.Errorf("compiled guest image catalog trust binding does not match")
	}
	return string(decoded), nil
}
