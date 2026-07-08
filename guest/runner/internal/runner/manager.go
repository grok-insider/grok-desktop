package runner

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/grok-insider/grok-desktop/guest/runner/internal/strictjson"
	jsonschema "github.com/santhosh-tekuri/jsonschema/v5"
)

const (
	maxIntegrations      = 16
	maxRestartAttempts   = 5
	catalogStateFileName = "catalog-state.json"
)

var integrationIDPattern = regexp.MustCompile(`^[a-z][a-z0-9]*(?:[.-][a-z0-9]+)+$`)
var mountIDPattern = regexp.MustCompile(`^[a-z][a-z0-9.-]{0,62}$`)

type Workspace struct {
	MountID  string `json:"mountId"`
	Path     string `json:"path"`
	ReadOnly bool   `json:"readOnly"`
}

type IntegrationStatus struct {
	ID      string `json:"id"`
	Version string `json:"version"`
	State   string `json:"state"`
}

type Manager struct {
	policy            Policy
	verifier          *CatalogVerifier
	computerUseSchema *jsonschema.Schema

	mu        sync.Mutex
	closed    bool
	updating  bool
	catalog   *VerifiedCatalog
	catalogID catalogState
	instances map[string]*managedInstance
	pending   map[string]*pendingStart
}

type pendingStart struct {
	bundle *VerifiedBundle
	cancel context.CancelFunc
	done   chan struct{}
}

type catalogState struct {
	Version  int    `json:"version"`
	Revision uint64 `json:"revision"`
	SHA256   string `json:"sha256"`
}

type managedInstance struct {
	policy         Policy
	bundle         *VerifiedBundle
	initialization initializeParams
	stateDirectory string
	grants         map[string]struct{}
	context        context.Context
	cancel         context.CancelFunc

	mu      sync.RWMutex
	process *adapterProcess
	state   string
	stop    chan string
	done    chan struct{}
}

func NewManager(policy Policy, trust TrustStore) (*Manager, error) {
	verifier, err := NewCatalogVerifier(policy, trust)
	if err != nil {
		return nil, err
	}
	schemaData, err := os.ReadFile(policy.ComputerUseSchema)
	if err != nil {
		verifier.Close()
		return nil, errors.New("computer-use schema is unavailable")
	}
	computerUseSchema, err := compileSchema(schemaData, "mem:///computer-use-v1.schema.json")
	if err != nil {
		verifier.Close()
		return nil, err
	}
	manager := &Manager{
		policy: policy, verifier: verifier, computerUseSchema: computerUseSchema,
		instances: make(map[string]*managedInstance), pending: make(map[string]*pendingStart),
	}
	state, err := loadCatalogState(policy.StateRoot)
	if err != nil {
		verifier.Close()
		return nil, err
	}
	manager.catalogID = state
	return manager, nil
}

func (manager *Manager) ApplyCatalog(ctx context.Context, data []byte) error {
	digest := sha256.Sum256(data)
	digestText := hex.EncodeToString(digest[:])
	verified, err := manager.verifier.Verify(data)
	if err != nil {
		return errors.New("catalog verification failed")
	}

	manager.mu.Lock()
	if manager.closed || manager.updating {
		manager.mu.Unlock()
		verified.Close()
		return errors.New("runner is not accepting a catalog update")
	}
	if verified.Revision < manager.catalogID.Revision ||
		(verified.Revision == manager.catalogID.Revision && manager.catalogID.SHA256 != "" && manager.catalogID.SHA256 != digestText) {
		manager.mu.Unlock()
		verified.Close()
		return errors.New("catalog rollback or revision reuse was rejected")
	}
	if verified.Revision == manager.catalogID.Revision && manager.catalogID.SHA256 == digestText && manager.catalog != nil {
		manager.mu.Unlock()
		verified.Close()
		return nil
	}
	manager.updating = true
	pending := make([]*pendingStart, 0, len(manager.pending))
	for _, start := range manager.pending {
		pending = append(pending, start)
		start.cancel()
	}
	manager.mu.Unlock()
	for _, start := range pending {
		select {
		case <-start.done:
		case <-ctx.Done():
			manager.mu.Lock()
			manager.updating = false
			manager.mu.Unlock()
			verified.Close()
			return ctx.Err()
		}
	}

	manager.mu.Lock()
	instances := manager.instances
	manager.instances = make(map[string]*managedInstance)
	manager.mu.Unlock()

	for _, instance := range instances {
		if err := instance.stopAndWait(ctx, "update"); err != nil {
			manager.mu.Lock()
			manager.updating = false
			manager.mu.Unlock()
			verified.Close()
			return err
		}
	}
	state := catalogState{Version: 1, Revision: verified.Revision, SHA256: digestText}
	if err := persistCatalogState(manager.policy.StateRoot, state); err != nil {
		manager.mu.Lock()
		manager.updating = false
		manager.mu.Unlock()
		verified.Close()
		return err
	}

	manager.mu.Lock()
	previous := manager.catalog
	manager.catalog = verified
	manager.catalogID = state
	manager.updating = false
	manager.mu.Unlock()
	if previous != nil {
		previous.Close()
	}
	return nil
}

func (manager *Manager) Start(
	ctx context.Context,
	integrationID string,
	config json.RawMessage,
	grants []string,
	workspaces []Workspace,
) error {
	manager.mu.Lock()
	if manager.closed || manager.updating || manager.catalog == nil {
		manager.mu.Unlock()
		return errors.New("verified catalog is unavailable")
	}
	if len(manager.instances)+len(manager.pending) >= maxIntegrations {
		manager.mu.Unlock()
		return errors.New("integration capacity is exhausted")
	}
	if manager.instances[integrationID] != nil || manager.pending[integrationID] != nil {
		manager.mu.Unlock()
		return errors.New("integration is already running")
	}
	bundle := manager.catalog.Bundles[integrationID]
	if bundle == nil {
		manager.mu.Unlock()
		return errors.New("integration is not present in the verified catalog")
	}
	startContext, cancelStart := context.WithCancel(ctx)
	pending := &pendingStart{bundle: bundle, cancel: cancelStart, done: make(chan struct{})}
	manager.pending[integrationID] = pending
	manager.mu.Unlock()
	defer func() {
		cancelStart()
		manager.mu.Lock()
		if manager.pending[integrationID] == pending {
			delete(manager.pending, integrationID)
		}
		close(pending.done)
		manager.mu.Unlock()
	}()

	configuration, err := validateConfiguration(bundle.ConfigSchema, config)
	if err != nil {
		return err
	}
	granted, normalizedGrants, err := validateGrants(bundle, grants)
	if err != nil {
		return err
	}
	if err := validateWorkspaces(manager.policy.WorkspaceRoot, workspaces); err != nil {
		return err
	}
	stateDirectory, err := ensureIntegrationState(manager.policy.StateRoot, integrationID)
	if err != nil {
		return err
	}
	initialization := initializeParams{
		IntegrationID: integrationID, IntegrationVersion: bundle.Manifest.Version,
		ProtocolVersion: protocolVersion, Config: configuration, Grants: normalizedGrants,
		Workspaces: append([]Workspace(nil), workspaces...),
	}
	instanceContext, cancelInstance := context.WithCancel(context.Background())
	process, err := launchAdapter(instanceContext, startContext, manager.policy, bundle, stateDirectory, initialization)
	if err != nil {
		cancelInstance()
		return errors.New("integration adapter could not be initialized")
	}
	instance := &managedInstance{
		policy: manager.policy, bundle: bundle, initialization: initialization,
		stateDirectory: stateDirectory, grants: granted, process: process, state: "ready",
		context: instanceContext, cancel: cancelInstance,
		stop: make(chan string, 1), done: make(chan struct{}),
	}

	manager.mu.Lock()
	if manager.closed || manager.updating || manager.catalog == nil || manager.catalog.Bundles[integrationID] != bundle ||
		manager.instances[integrationID] != nil || manager.pending[integrationID] != pending {
		manager.mu.Unlock()
		stopContext, cancel := context.WithTimeout(context.Background(), time.Duration(bundle.Manifest.Lifecycle.ShutdownTimeoutMS)*time.Millisecond)
		defer cancel()
		process.shutdown(stopContext, "update")
		cancelInstance()
		return errors.New("integration start raced with a catalog change")
	}
	manager.instances[integrationID] = instance
	manager.mu.Unlock()
	go instance.supervise()
	return nil
}

func (manager *Manager) Stop(ctx context.Context, integrationID, reason string) error {
	if !validShutdownReason(reason) {
		return errors.New("integration shutdown reason is invalid")
	}
	manager.mu.Lock()
	instance := manager.instances[integrationID]
	if instance != nil {
		delete(manager.instances, integrationID)
	}
	manager.mu.Unlock()
	if instance == nil {
		return errors.New("integration is not running")
	}
	return instance.stopAndWait(ctx, reason)
}

func (manager *Manager) Call(ctx context.Context, integrationID, method string, params json.RawMessage) (json.RawMessage, error) {
	manager.mu.Lock()
	instance := manager.instances[integrationID]
	manager.mu.Unlock()
	if instance == nil {
		return nil, errors.New("integration is not running")
	}
	if err := authorizeComputerUse(instance.grants, method, params); err != nil {
		return nil, err
	}
	if method == "computer-use.act" {
		if err := validateSchemaValue(manager.computerUseSchema, params); err != nil {
			return nil, errors.New("computer-use action is invalid")
		}
	} else {
		if method != "computer-use.observe" || decodeEmptyParams(params) != nil {
			return nil, errors.New("computer-use method or parameters are invalid")
		}
		params = json.RawMessage(`{}`)
	}
	result, err := instance.call(ctx, method, params)
	if err != nil {
		return nil, err
	}
	if err := validateSchemaValue(manager.computerUseSchema, result); err != nil {
		return nil, errors.New("computer-use response is invalid")
	}
	if err := validateComputerUseResponse(method, params, result); err != nil {
		return nil, err
	}
	return result, nil
}

func (manager *Manager) Statuses() (uint64, []IntegrationStatus) {
	manager.mu.Lock()
	revision := manager.catalogID.Revision
	instances := make([]*managedInstance, 0, len(manager.instances))
	for _, instance := range manager.instances {
		instances = append(instances, instance)
	}
	manager.mu.Unlock()
	statuses := make([]IntegrationStatus, 0, len(instances))
	for _, instance := range instances {
		instance.mu.RLock()
		statuses = append(statuses, IntegrationStatus{ID: instance.bundle.Manifest.ID, Version: instance.bundle.Manifest.Version, State: instance.state})
		instance.mu.RUnlock()
	}
	sort.Slice(statuses, func(left, right int) bool { return statuses[left].ID < statuses[right].ID })
	return revision, statuses
}

func (manager *Manager) Close(ctx context.Context) error {
	manager.mu.Lock()
	if manager.closed {
		manager.mu.Unlock()
		return nil
	}
	manager.closed = true
	pending := make([]*pendingStart, 0, len(manager.pending))
	for _, start := range manager.pending {
		pending = append(pending, start)
		start.cancel()
	}
	instances := manager.instances
	manager.instances = make(map[string]*managedInstance)
	catalog := manager.catalog
	manager.catalog = nil
	manager.mu.Unlock()
	for _, start := range pending {
		select {
		case <-start.done:
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	for _, instance := range instances {
		if err := instance.stopAndWait(ctx, "guest-shutdown"); err != nil {
			return err
		}
	}
	if catalog != nil {
		catalog.Close()
	}
	manager.verifier.Close()
	return ctx.Err()
}

func (instance *managedInstance) call(ctx context.Context, method string, params json.RawMessage) (json.RawMessage, error) {
	instance.mu.RLock()
	process := instance.process
	state := instance.state
	instance.mu.RUnlock()
	if process == nil || state != "ready" {
		return nil, errors.New("integration is not ready")
	}
	return process.call(ctx, method, params)
}

func (instance *managedInstance) supervise() {
	defer close(instance.done)
	defer instance.cancel()
	health := instance.bundle.Manifest.Lifecycle.HealthCheck
	ticker := time.NewTicker(time.Duration(health.IntervalMS) * time.Millisecond)
	defer ticker.Stop()
	failures := 0
	restarts := 0
	for {
		instance.mu.RLock()
		process := instance.process
		instance.mu.RUnlock()
		select {
		case reason := <-instance.stop:
			instance.mu.Lock()
			instance.state = "stopping"
			instance.mu.Unlock()
			ctx, cancel := context.WithTimeout(context.Background(), time.Duration(instance.bundle.Manifest.Lifecycle.ShutdownTimeoutMS)*time.Millisecond)
			process.shutdown(ctx, reason)
			cancel()
			instance.mu.Lock()
			instance.process = nil
			instance.state = "stopped"
			instance.mu.Unlock()
			return
		case <-instance.context.Done():
			process.kill()
			instance.mu.Lock()
			instance.process = nil
			instance.state = "stopped"
			instance.mu.Unlock()
			return
		case <-process.done:
			failures = health.FailureThreshold
		case <-ticker.C:
			ctx, cancel := context.WithTimeout(context.Background(), time.Duration(health.TimeoutMS)*time.Millisecond)
			err := process.health(ctx)
			cancel()
			if err == nil {
				failures = 0
				continue
			}
			failures++
		}
		if failures < health.FailureThreshold {
			continue
		}
		process.kill()
		if instance.bundle.Manifest.Lifecycle.RestartPolicy == "never" || restarts >= maxRestartAttempts {
			instance.mu.Lock()
			instance.process = nil
			instance.state = "failed"
			instance.mu.Unlock()
			return
		}
		restarts++
		instance.mu.Lock()
		instance.state = "restarting"
		instance.mu.Unlock()
		backoff := time.Duration(1<<min(restarts-1, 4)) * time.Second
		select {
		case reason := <-instance.stop:
			instance.mu.Lock()
			instance.process = nil
			instance.state = "stopped"
			instance.mu.Unlock()
			_ = reason
			return
		case <-instance.context.Done():
			instance.mu.Lock()
			instance.process = nil
			instance.state = "stopped"
			instance.mu.Unlock()
			return
		case <-time.After(backoff):
		}
		ctx, cancel := context.WithTimeout(instance.context, time.Duration(instance.bundle.Adapter.Limits.InitializeTimeoutMS)*time.Millisecond)
		replacement, err := launchAdapter(instance.context, ctx, instance.policy, instance.bundle, instance.stateDirectory, instance.initialization)
		cancel()
		if err != nil {
			failures = health.FailureThreshold
			continue
		}
		instance.mu.Lock()
		instance.process = replacement
		instance.state = "ready"
		instance.mu.Unlock()
		failures = 0
	}
}

func (instance *managedInstance) stopAndWait(ctx context.Context, reason string) error {
	select {
	case instance.stop <- reason:
	default:
	}
	select {
	case <-instance.done:
		return nil
	case <-ctx.Done():
		instance.cancel()
		instance.mu.RLock()
		process := instance.process
		instance.mu.RUnlock()
		if process != nil {
			process.kill()
		}
		select {
		case <-instance.done:
		case <-time.After(5 * time.Second):
			return errors.New("integration supervisor did not stop after cancellation")
		}
		return ctx.Err()
	}
}

func validateConfiguration(schema *jsonschema.Schema, raw json.RawMessage) (json.RawMessage, error) {
	var value any
	if err := strictjson.Decode(raw, 256<<10, &value); err != nil {
		return nil, errors.New("integration configuration is invalid")
	}
	if _, ok := value.(map[string]any); !ok || schema.Validate(value) != nil {
		return nil, errors.New("integration configuration does not match its schema")
	}
	return append(json.RawMessage(nil), raw...), nil
}

func validateSchemaValue(schema *jsonschema.Schema, raw json.RawMessage) error {
	var value any
	if err := strictjson.Decode(raw, 16<<20, &value); err != nil {
		return err
	}
	return schema.Validate(value)
}

type computerUseCorrelation struct {
	Protocol            string                    `json:"protocol"`
	Type                string                    `json:"type"`
	ActionID            string                    `json:"actionId"`
	ObservationRevision uint64                    `json:"observationRevision"`
	Application         computerUseApplicationRef `json:"application"`
}

type computerUseApplicationRef struct {
	ApplicationID string  `json:"applicationId"`
	InstanceID    string  `json:"instanceId"`
	ProcessID     *uint64 `json:"processId"`
	WindowID      *string `json:"windowId"`
}

func validateComputerUseResponse(method string, request, response json.RawMessage) error {
	var actual computerUseCorrelation
	if err := json.Unmarshal(response, &actual); err != nil || actual.Protocol != "grok.computer-use/v1" {
		return errors.New("computer-use response correlation is invalid")
	}
	switch method {
	case "computer-use.observe":
		if actual.Type != "observation" {
			return errors.New("computer-use observation response has the wrong type")
		}
		return nil
	case "computer-use.act":
		var expected computerUseCorrelation
		if err := json.Unmarshal(request, &expected); err != nil || expected.Protocol != "grok.computer-use/v1" ||
			expected.Type != "action" || actual.Type != "action-result" || actual.ActionID != expected.ActionID ||
			actual.ObservationRevision != expected.ObservationRevision || !reflect.DeepEqual(actual.Application, expected.Application) {
			return errors.New("computer-use action response does not match its request")
		}
		return nil
	default:
		return errors.New("computer-use response method is invalid")
	}
}

func validateGrants(bundle *VerifiedBundle, grants []string) (map[string]struct{}, []string, error) {
	if len(grants) > len(bundle.Manifest.Capabilities) {
		return nil, nil, errors.New("grant set exceeds manifest capabilities")
	}
	allowed := make(map[string]struct{}, len(bundle.Manifest.Capabilities))
	for _, capability := range bundle.Manifest.Capabilities {
		allowed[capability] = struct{}{}
	}
	result := make(map[string]struct{}, len(grants))
	for _, grant := range grants {
		if _, exists := allowed[grant]; !exists {
			return nil, nil, errors.New("grant is outside manifest capabilities")
		}
		if _, duplicate := result[grant]; duplicate {
			return nil, nil, errors.New("grant is duplicated")
		}
		result[grant] = struct{}{}
	}
	normalized := append([]string(nil), grants...)
	sort.Strings(normalized)
	return result, normalized, nil
}

func validateWorkspaces(root string, workspaces []Workspace) error {
	if len(workspaces) > 32 {
		return errors.New("workspace grant count exceeds limit")
	}
	seen := make(map[string]struct{}, len(workspaces))
	for _, workspace := range workspaces {
		clean := filepath.Clean(workspace.Path)
		expected := filepath.Join(root, workspace.MountID)
		if !mountIDPattern.MatchString(workspace.MountID) || !workspace.ReadOnly ||
			!filepath.IsAbs(clean) || clean != workspace.Path || clean != expected {
			return errors.New("workspace grant is invalid")
		}
		if _, duplicate := seen[workspace.MountID]; duplicate {
			return errors.New("workspace mount identity is duplicated")
		}
		seen[workspace.MountID] = struct{}{}
	}
	return nil
}

func ensureIntegrationState(root, integrationID string) (string, error) {
	if len(integrationID) > 128 || !integrationIDPattern.MatchString(integrationID) {
		return "", errors.New("integration state identity is invalid")
	}
	state := filepath.Join(root, integrationID)
	relative, err := filepath.Rel(root, state)
	if err != nil || relative != integrationID || strings.ContainsAny(integrationID, `/\\`) {
		return "", errors.New("integration state identity is invalid")
	}
	if err := os.Mkdir(state, 0o700); err != nil && !errors.Is(err, os.ErrExist) {
		return "", errors.New("integration state could not be created")
	}
	info, err := os.Lstat(state)
	if err != nil || !info.IsDir() || info.Mode().Perm()&0o077 != 0 || info.Mode()&os.ModeSymlink != 0 {
		return "", errors.New("integration state directory is unsafe")
	}
	return state, nil
}

func authorizeComputerUse(grants map[string]struct{}, method string, params json.RawMessage) error {
	required := ""
	switch method {
	case "computer-use.observe":
		required = "computer-use.observe"
	case "computer-use.act":
		var envelope struct {
			Action struct {
				Kind string `json:"kind"`
			} `json:"action"`
		}
		// Full action envelopes contain additional schema-validated fields. Decode
		// here only to select the capability after strict JSON validation.
		if err := strictjson.Validate(params, 16<<20); err != nil || json.Unmarshal(params, &envelope) != nil {
			return errors.New("computer-use action is invalid")
		}
		switch envelope.Action.Kind {
		case "pointer.move", "pointer.click", "pointer.drag", "scroll":
			required = "computer-use.pointer"
		case "key.press", "text.input":
			required = "computer-use.keyboard"
		case "wait":
			required = "computer-use.wait"
		default:
			return errors.New("computer-use action kind is invalid")
		}
	default:
		return errors.New("computer-use method is invalid")
	}
	if _, allowed := grants[required]; !allowed {
		return errors.New("computer-use grant is absent")
	}
	return nil
}

func validShutdownReason(reason string) bool {
	switch reason {
	case "user", "update", "uninstall", "guest-shutdown", "health-failure":
		return true
	default:
		return false
	}
}

func loadCatalogState(root string) (catalogState, error) {
	data, err := os.ReadFile(filepath.Join(root, catalogStateFileName))
	if errors.Is(err, os.ErrNotExist) {
		return catalogState{Version: 1}, nil
	}
	if err != nil {
		return catalogState{}, errors.New("catalog state could not be read")
	}
	var state catalogState
	if err := strictjson.Decode(data, 4096, &state); err != nil || state.Version != 1 || state.Revision == 0 || !digestPattern.MatchString(state.SHA256) {
		return catalogState{}, errors.New("catalog state is invalid")
	}
	return state, nil
}

func persistCatalogState(root string, state catalogState) error {
	data, err := json.Marshal(state)
	if err != nil {
		return errors.New("catalog state could not be encoded")
	}
	temporary, err := os.CreateTemp(root, ".catalog-state-*")
	if err != nil {
		return errors.New("catalog state could not be staged")
	}
	temporaryName := temporary.Name()
	defer os.Remove(temporaryName)
	if err := temporary.Chmod(0o600); err != nil {
		temporary.Close()
		return errors.New("catalog state permissions could not be set")
	}
	if _, err := temporary.Write(append(data, '\n')); err != nil || temporary.Sync() != nil || temporary.Close() != nil {
		return errors.New("catalog state could not be persisted")
	}
	if err := os.Rename(temporaryName, filepath.Join(root, catalogStateFileName)); err != nil {
		return errors.New("catalog state could not be published")
	}
	directory, err := os.Open(root)
	if err != nil {
		return errors.New("catalog state directory could not be opened")
	}
	err = directory.Sync()
	directory.Close()
	if err != nil {
		return errors.New("catalog state directory could not be synchronized")
	}
	return nil
}

func (manager *Manager) String() string {
	revision, statuses := manager.Statuses()
	return fmt.Sprintf("guest integration manager revision=%d integrations=%d", revision, len(statuses))
}
