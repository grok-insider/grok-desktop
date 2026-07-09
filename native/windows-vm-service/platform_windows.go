//go:build windows

package vmservice

import (
	"context"
	"time"

	"github.com/grok-insider/grok-desktop/native/windows-vm-service/internal/hcsapi"
)

// NewPlatformService probes HCS before exposing the privileged endpoint. A host
// without VirtualMachinePlatform support remains unavailable; it never falls
// back to the simulator on Windows.
func NewPlatformService(config Config) (Service, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	return NewPlatformServiceContext(ctx, config)
}

// NewPlatformServiceContext lets the SCM host cancel tenant initialization
// during stop or preshutdown without leaving an untracked HCS reconciliation.
func NewPlatformServiceContext(ctx context.Context, config Config) (Service, error) {
	return newHCSService(ctx, config, hcsapi.NewClient(), newNativePathValidator())
}
