package host

import (
	"context"
	"fmt"

	vmservice "github.com/grok-insider/grok-desktop/native/windows-vm-service"
	"github.com/grok-insider/grok-desktop/native/windows-vm-service/transport"
)

// ServiceResolver selects a tenant-scoped backend from an authenticated peer.
// Production resolvers must never derive tenancy from a request payload.
type ServiceResolver interface {
	Resolve(context.Context, transport.PeerIdentity) (vmservice.Service, error)
}

type staticServiceResolver struct {
	service vmservice.Service
}

func (r staticServiceResolver) Resolve(context.Context, transport.PeerIdentity) (vmservice.Service, error) {
	if r.service == nil {
		return nil, fmt.Errorf("service is unavailable")
	}
	return r.service, nil
}
