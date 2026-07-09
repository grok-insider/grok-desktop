package tenant

import (
	"context"
	"errors"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	vmservice "github.com/grok-insider/grok-desktop/native/windows-vm-service"
	"github.com/grok-insider/grok-desktop/native/windows-vm-service/transport"
)

const (
	tenantTestSIDOne = "S-1-5-21-1000-1001-1002-1003"
	tenantTestSIDTwo = "S-1-5-21-2000-2001-2002-2003"
)

func TestDeriveStorageRootsUsesOpaqueDisjointTenantPath(t *testing.T) {
	dataRoot := filepath.Join(t.TempDir(), "service-data")
	one, err := DeriveStorageRoots(dataRoot, tenantTestSIDOne)
	if err != nil {
		t.Fatalf("DeriveStorageRoots: %v", err)
	}
	two, err := DeriveStorageRoots(dataRoot, tenantTestSIDTwo)
	if err != nil {
		t.Fatalf("DeriveStorageRoots second tenant: %v", err)
	}
	if strings.Contains(strings.ToUpper(one.TenantRoot), tenantTestSIDOne) {
		t.Fatalf("tenant path contains the SID: %q", one.TenantRoot)
	}
	if one.TenantRoot == two.TenantRoot {
		t.Fatal("different SIDs received one tenant root")
	}
	if filepath.Dir(one.ImageRoot) != one.TenantRoot || filepath.Dir(one.WorkspaceRoot) != one.TenantRoot {
		t.Fatalf("roots are not tenant scoped: %#v", one)
	}
}

func TestManagerCreatesOneBackendPerAuthenticatedTenant(t *testing.T) {
	var calls atomic.Int32
	factory := BackendFactoryFunc(func(ctx context.Context, config vmservice.Config) (vmservice.Service, error) {
		calls.Add(1)
		return vmservice.NewPlatformServiceContext(ctx, config)
	})
	manager := newTestManager(t, 2, factory)
	t.Cleanup(func() { _ = manager.Close(context.Background()) })

	first, err := manager.Resolve(context.Background(), developmentIdentity(tenantTestSIDOne))
	if err != nil {
		t.Fatalf("resolve first tenant: %v", err)
	}
	again, err := manager.Resolve(context.Background(), developmentIdentity(tenantTestSIDOne))
	if err != nil {
		t.Fatalf("resolve first tenant again: %v", err)
	}
	second, err := manager.Resolve(context.Background(), developmentIdentity(tenantTestSIDTwo))
	if err != nil {
		t.Fatalf("resolve second tenant: %v", err)
	}
	if first != again || first == second {
		t.Fatal("tenant backend identity was not stable and isolated")
	}
	if calls.Load() != 2 {
		t.Fatalf("factory calls = %d, want 2", calls.Load())
	}

	_, err = first.GetCapabilities(context.Background(), vmservice.GetCapabilitiesRequest{
		Request: vmservice.RequestIdentity{RequestID: "cross-tenant", UserSID: tenantTestSIDTwo},
	})
	if err == nil {
		t.Fatal("tenant backend accepted another authenticated SID")
	}
}

func TestManagerCoalescesConcurrentInitialization(t *testing.T) {
	var calls atomic.Int32
	release := make(chan struct{})
	factory := BackendFactoryFunc(func(ctx context.Context, config vmservice.Config) (vmservice.Service, error) {
		calls.Add(1)
		select {
		case <-release:
			return vmservice.NewPlatformServiceContext(ctx, config)
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	})
	manager := newTestManager(t, 2, factory)
	t.Cleanup(func() { _ = manager.Close(context.Background()) })

	var wait sync.WaitGroup
	errors := make(chan error, 8)
	for range 8 {
		wait.Add(1)
		go func() {
			defer wait.Done()
			_, err := manager.Resolve(context.Background(), developmentIdentity(tenantTestSIDOne))
			errors <- err
		}()
	}
	for calls.Load() == 0 {
		time.Sleep(time.Millisecond)
	}
	close(release)
	wait.Wait()
	close(errors)
	for err := range errors {
		if err != nil {
			t.Fatalf("concurrent Resolve: %v", err)
		}
	}
	if calls.Load() != 1 {
		t.Fatalf("factory calls = %d, want 1", calls.Load())
	}
}

func TestManagerBoundsTenantsAndRejectsUnprovedIdentity(t *testing.T) {
	manager := newTestManager(t, 1, BackendFactoryFunc(vmservice.NewPlatformServiceContext))
	t.Cleanup(func() { _ = manager.Close(context.Background()) })
	if _, err := manager.Resolve(context.Background(), developmentIdentity(tenantTestSIDOne)); err != nil {
		t.Fatalf("resolve first tenant: %v", err)
	}
	if _, err := manager.Resolve(context.Background(), developmentIdentity(tenantTestSIDTwo)); err == nil {
		t.Fatal("manager exceeded tenant capacity")
	}
	if _, err := manager.Resolve(context.Background(), transport.PeerIdentity{UserSID: tenantTestSIDOne}); err == nil {
		t.Fatal("manager accepted an identity without a transport proof")
	}
}

func TestManagerCloseCancelsInitialization(t *testing.T) {
	started := make(chan struct{})
	factory := BackendFactoryFunc(func(ctx context.Context, _ vmservice.Config) (vmservice.Service, error) {
		close(started)
		<-ctx.Done()
		return nil, ctx.Err()
	})
	manager := newTestManager(t, 1, factory)
	resolved := make(chan error, 1)
	go func() {
		_, err := manager.Resolve(context.Background(), developmentIdentity(tenantTestSIDOne))
		resolved <- err
	}()
	<-started
	closeContext, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if err := manager.Close(closeContext); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := <-resolved; err == nil {
		t.Fatal("in-flight tenant initialization succeeded after Close")
	}
}

func TestManagerCloseClosesResolvedTenantServices(t *testing.T) {
	manager := newTestManager(t, 1, BackendFactoryFunc(vmservice.NewPlatformServiceContext))
	service, err := manager.Resolve(context.Background(), developmentIdentity(tenantTestSIDOne))
	if err != nil {
		t.Fatalf("Resolve: %v", err)
	}
	if err := manager.Close(context.Background()); err != nil {
		t.Fatalf("Close: %v", err)
	}
	_, err = service.GetCapabilities(context.Background(), vmservice.GetCapabilitiesRequest{
		Request: vmservice.RequestIdentity{RequestID: "after-close", UserSID: tenantTestSIDOne},
	})
	var serviceErr *vmservice.Error
	if !errors.As(err, &serviceErr) || serviceErr.Code != vmservice.CodeUnavailable {
		t.Fatalf("closed tenant result = %v, want unavailable", err)
	}
}

func newTestManager(t *testing.T, maxTenants int, factory BackendFactory) *Manager {
	t.Helper()
	manager, err := NewManager(Config{
		DataRoot: filepath.Join(t.TempDir(), "data"), MaxTenants: maxTenants,
		AllowDevelopmentIdentities: true, Factory: factory,
	})
	if err != nil {
		t.Fatalf("NewManager: %v", err)
	}
	return manager
}

func developmentIdentity(sid string) transport.PeerIdentity {
	return transport.PeerIdentity{UserSID: sid, Method: transport.AuthenticationDevelopment}
}
