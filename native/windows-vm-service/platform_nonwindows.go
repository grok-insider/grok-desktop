//go:build !windows

package vmservice

import "context"

// NewPlatformService returns the stateful simulator off Windows. Callers must
// inspect Capabilities.Simulated and never treat it as an isolation boundary.
func NewPlatformService(config Config) (Service, error) {
	return NewStubService(config)
}

func NewPlatformServiceContext(ctx context.Context, config Config) (Service, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	return NewStubService(config)
}
