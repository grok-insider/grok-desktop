package tenant

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"path/filepath"
	"strings"
	"sync"

	vmservice "github.com/grok-insider/grok-desktop/native/windows-vm-service"
	"github.com/grok-insider/grok-desktop/native/windows-vm-service/transport"
)

const (
	DefaultMaxTenants = 32
	MaxTenantLimit    = 128
)

type BackendFactory interface {
	New(context.Context, vmservice.Config) (vmservice.Service, error)
}

type BackendFactoryFunc func(context.Context, vmservice.Config) (vmservice.Service, error)

func (factory BackendFactoryFunc) New(ctx context.Context, config vmservice.Config) (vmservice.Service, error) {
	return factory(ctx, config)
}

type Config struct {
	DataRoot                   string
	MaxTenants                 int
	AllowedSocketPurposes      []vmservice.SocketPurpose
	GuestControlMaxBytes       int
	GuestImagePolicy           *vmservice.GuestImagePolicy
	AllowDevelopmentIdentities bool
	Factory                    BackendFactory
}

type StorageRoots struct {
	TenantRoot    string
	ImageRoot     string
	StagingRoot   string
	WorkspaceRoot string
}

type entry struct {
	ready   chan struct{}
	service vmservice.Service
	err     error
	cancel  context.CancelFunc
}

// Manager owns a bounded set of tenant backends. Its only tenant key is the
// identity proved by the transport; request payloads never participate.
type Manager struct {
	config Config
	// prepareStorage is fixed to the audited platform implementation in
	// production. Keeping it internal lets manager unit tests exercise tenant
	// coordination without provisioning host ACLs or requiring HCS.
	prepareStorage func(StorageRoots, string) error

	mu      sync.Mutex
	closed  bool
	entries map[string]*entry
	create  sync.WaitGroup
}

func NewManager(config Config) (*Manager, error) {
	if config.Factory == nil {
		return nil, fmt.Errorf("tenant backend factory is required")
	}
	if config.MaxTenants == 0 {
		config.MaxTenants = DefaultMaxTenants
	}
	if config.MaxTenants < 1 || config.MaxTenants > MaxTenantLimit {
		return nil, fmt.Errorf("MaxTenants must be between 1 and %d", MaxTenantLimit)
	}
	if !filepath.IsAbs(config.DataRoot) || filepath.Clean(config.DataRoot) == string(filepath.Separator) {
		return nil, fmt.Errorf("DataRoot must be an absolute service-owned directory")
	}
	config.DataRoot = filepath.Clean(config.DataRoot)
	return &Manager{
		config:         config,
		prepareStorage: secureTenantStorage,
		entries:        make(map[string]*entry),
	}, nil
}

func (manager *Manager) Resolve(ctx context.Context, identity transport.PeerIdentity) (vmservice.Service, error) {
	if err := transport.ValidatePeerIdentity(identity, manager.config.AllowDevelopmentIdentities); err != nil {
		return nil, &vmservice.Error{Code: vmservice.CodePermissionDenied, Message: "authenticated user is not eligible for VM access"}
	}
	key := strings.ToUpper(identity.UserSID)

	manager.mu.Lock()
	if manager.closed {
		manager.mu.Unlock()
		return nil, &vmservice.Error{Code: vmservice.CodeUnavailable, Message: "tenant manager is stopping"}
	}
	if existing := manager.entries[key]; existing != nil {
		manager.mu.Unlock()
		return waitForEntry(ctx, existing)
	}
	if len(manager.entries) >= manager.config.MaxTenants {
		manager.mu.Unlock()
		return nil, &vmservice.Error{Code: vmservice.CodeUnavailable, Message: "tenant capacity is exhausted"}
	}
	creationContext, cancel := context.WithCancel(ctx)
	created := &entry{ready: make(chan struct{}), cancel: cancel}
	manager.entries[key] = created
	manager.create.Add(1)
	manager.mu.Unlock()

	service, err := manager.createBackend(creationContext, identity.UserSID)
	cancel()
	var discard vmservice.Service
	manager.mu.Lock()
	if manager.closed && err == nil {
		discard = service
		service = nil
		err = context.Canceled
	}
	created.service = service
	created.err = err
	created.cancel = nil
	if err != nil && manager.entries[key] == created {
		delete(manager.entries, key)
	}
	close(created.ready)
	manager.mu.Unlock()
	if discard != nil {
		_ = discard.Close(context.Background())
	}
	manager.create.Done()
	if err != nil {
		return nil, &vmservice.Error{Code: vmservice.CodeUnavailable, Message: "tenant backend could not be initialized"}
	}
	return service, nil
}

func (manager *Manager) createBackend(ctx context.Context, sid string) (vmservice.Service, error) {
	roots, err := DeriveStorageRoots(manager.config.DataRoot, sid)
	if err != nil {
		return nil, err
	}
	if err := manager.prepareStorage(roots, sid); err != nil {
		return nil, err
	}
	return manager.config.Factory.New(ctx, vmservice.Config{
		CurrentUserSID: sid,
		ImageRoot:      roots.ImageRoot, WorkspaceRoot: roots.WorkspaceRoot,
		AllowedSocketPurposes: append([]vmservice.SocketPurpose(nil), manager.config.AllowedSocketPurposes...),
		GuestControlMaxBytes:  manager.config.GuestControlMaxBytes,
		GuestImagePolicy:      manager.config.GuestImagePolicy,
	})
}

// PrepareServiceStorage establishes the service-owned root before global
// rollback state is read or written and before any tenant is admitted.
func PrepareServiceStorage(dataRoot string) error {
	if !filepath.IsAbs(dataRoot) || filepath.Clean(dataRoot) == string(filepath.Separator) {
		return fmt.Errorf("data root is not an absolute service directory")
	}
	return secureServiceStorageRoot(filepath.Clean(dataRoot))
}

func waitForEntry(ctx context.Context, existing *entry) (vmservice.Service, error) {
	select {
	case <-existing.ready:
		if existing.err != nil || existing.service == nil {
			return nil, &vmservice.Error{Code: vmservice.CodeUnavailable, Message: "tenant backend could not be initialized"}
		}
		return existing.service, nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

// Close blocks new tenants, cancels initialization, and waits for factories to
// leave their recovery-safe boundary. Durable VMs are intentionally not
// deleted or force-stopped; the next service start reconciles them.
func (manager *Manager) Close(ctx context.Context) error {
	manager.mu.Lock()
	if !manager.closed {
		manager.closed = true
		for _, tenant := range manager.entries {
			if tenant.cancel != nil {
				tenant.cancel()
			}
		}
	}
	manager.mu.Unlock()

	done := make(chan struct{})
	go func() {
		manager.create.Wait()
		close(done)
	}()
	select {
	case <-done:
		manager.mu.Lock()
		services := make([]vmservice.Service, 0, len(manager.entries))
		for _, tenant := range manager.entries {
			if tenant.service != nil {
				services = append(services, tenant.service)
			}
		}
		manager.mu.Unlock()
		for _, service := range services {
			if err := ctx.Err(); err != nil {
				return err
			}
			if err := service.Close(ctx); err != nil {
				return err
			}
		}
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func DeriveStorageRoots(dataRoot, sid string) (StorageRoots, error) {
	if !filepath.IsAbs(dataRoot) || filepath.Clean(dataRoot) == string(filepath.Separator) {
		return StorageRoots{}, fmt.Errorf("data root is not an absolute service directory")
	}
	if err := transport.ValidatePeerIdentity(transport.PeerIdentity{
		UserSID: sid, Method: transport.AuthenticationDevelopment,
	}, true); err != nil {
		return StorageRoots{}, fmt.Errorf("tenant identity is invalid")
	}
	digest := sha256.Sum256([]byte(strings.ToUpper(sid)))
	tenantRoot := filepath.Join(filepath.Clean(dataRoot), "tenants", hex.EncodeToString(digest[:]))
	imageRoot := filepath.Join(tenantRoot, "images")
	return StorageRoots{
		TenantRoot: tenantRoot, ImageRoot: imageRoot,
		StagingRoot:   filepath.Join(imageRoot, "staging"),
		WorkspaceRoot: filepath.Join(tenantRoot, "workspaces"),
	}, nil
}
