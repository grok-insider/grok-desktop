//go:build windows

package main

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"time"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/svc"
)

func finalizePlatformOptions(options *options) error {
	if options.developmentUserSID != "" {
		return fmt.Errorf("--development-user-sid is unavailable on Windows")
	}
	if options.dataRoot != "" {
		return fmt.Errorf("--data-root cannot override the service-owned ProgramData root on Windows")
	}
	if options.endpoint != "" && options.endpoint != `\\.\pipe\GrokDesktop.VMService.v1` {
		return fmt.Errorf("--listen cannot override the production service pipe")
	}
	if options.releaseRoot != "" {
		return fmt.Errorf("--release-root is unavailable on Windows")
	}
	executable, err := os.Executable()
	if err != nil {
		return fmt.Errorf("resolve packaged service executable: %w", err)
	}
	serviceDirectory := filepath.Dir(executable)
	if !strings.EqualFold(filepath.Base(serviceDirectory), "service") {
		return fmt.Errorf("service executable is outside the packaged service directory")
	}
	options.releaseRoot = filepath.Dir(serviceDirectory)
	compiledTrust, err := decodeCompiledGuestCatalogTrust()
	if err != nil {
		return err
	}
	options.guestCatalogTrust = compiledTrust
	if options.guestCatalogTrust == "" {
		return fmt.Errorf("guest image catalog trust was not compiled into the service")
	}
	programData, err := windows.KnownFolderPath(windows.FOLDERID_ProgramData, windows.KF_FLAG_DEFAULT)
	if err != nil {
		return fmt.Errorf("resolve ProgramData: %w", err)
	}
	options.dataRoot = filepath.Join(programData, "Grok Desktop", "VM Service")
	return nil
}

func isServiceProcess() (bool, error)  { return svc.IsWindowsService() }
func explicitForegroundRequired() bool { return true }
func allowDevelopmentIdentities() bool { return false }

func runService(options options, logger *slog.Logger) error {
	handler := &windowsServiceHandler{
		logger: logger,
		serve: func(ctx context.Context) error {
			return runHost(ctx, options, logger)
		},
		shutdownTimeout: options.shutdownTimeout,
	}
	return svc.Run(serviceName, handler)
}

type windowsServiceHandler struct {
	logger          *slog.Logger
	serve           func(context.Context) error
	shutdownTimeout time.Duration
}

func (handler *windowsServiceHandler) Execute(
	_ []string,
	requests <-chan svc.ChangeRequest,
	statuses chan<- svc.Status,
) (bool, uint32) {
	statuses <- svc.Status{State: svc.StartPending, WaitHint: 30_000}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	result := make(chan error, 1)
	go func() { result <- handler.serve(ctx) }()

	accepted := svc.AcceptStop | svc.AcceptShutdown | svc.AcceptPreShutdown
	running := svc.Status{State: svc.Running, Accepts: accepted}
	statuses <- running
	stopping := false
	var stopDeadline <-chan time.Time
	var stopTimer *time.Timer
	beginStop := func() {
		if stopping {
			return
		}
		stopping = true
		statuses <- stopPendingStatus(handler.shutdownTimeout, 1)
		cancel()
		stopTimer = time.NewTimer(handler.shutdownTimeout + time.Second)
		stopDeadline = stopTimer.C
	}
	defer func() {
		if stopTimer != nil {
			stopTimer.Stop()
		}
	}()
	for {
		select {
		case err := <-result:
			if err != nil && !stopping {
				handler.logger.Error("VM service host failed")
				return true, 1
			}
			return false, 0
		case <-stopDeadline:
			handler.logger.Error("VM service shutdown exceeded its deadline")
			return true, 2
		case request, open := <-requests:
			if !open {
				beginStop()
				continue
			}
			switch request.Cmd {
			case svc.Interrogate:
				if stopping {
					statuses <- stopPendingStatus(handler.shutdownTimeout, 1)
				} else {
					statuses <- running
				}
			case svc.Stop, svc.Shutdown, svc.PreShutdown:
				beginStop()
			default:
				if !stopping {
					statuses <- running
				}
			}
		}
	}
}

func stopPendingStatus(timeout time.Duration, checkpoint uint32) svc.Status {
	waitHint := timeout.Milliseconds()
	if waitHint < 1_000 {
		waitHint = 1_000
	}
	if waitHint > int64(^uint32(0)) {
		waitHint = int64(^uint32(0))
	}
	return svc.Status{State: svc.StopPending, CheckPoint: checkpoint, WaitHint: uint32(waitHint)}
}
