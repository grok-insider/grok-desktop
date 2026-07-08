package runner

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"regexp"
	"sync"
	"time"

	"github.com/grok-insider/grok-desktop/guest/runner/internal/strictjson"
)

const (
	controlProtocol       = "grok.guest-control/v1"
	maxControlConnections = 16
	maxRequestLifetime    = 2 * time.Minute
	maxReplayEntries      = 1024
	maxReplayBytes        = 64 << 20
	maxReplayInflight     = 16
)

var requestIDPattern = regexp.MustCompile(`^[A-Za-z0-9._:-]{1,128}$`)

type controlManager interface {
	ApplyCatalog(context.Context, []byte) error
	Start(context.Context, string, json.RawMessage, []string, []Workspace) error
	Stop(context.Context, string, string) error
	Call(context.Context, string, string, json.RawMessage) (json.RawMessage, error)
	Statuses() (uint64, []IntegrationStatus)
}

type ControlServer struct {
	manager          controlManager
	mounter          workspaceMounter
	policy           Policy
	now              func() time.Time
	handshakeTimeout time.Duration

	replayMu       sync.Mutex
	replays        map[string]*replayEntry
	order          []string
	replayBytes    int
	replayInflight int
}

type workspaceMounter interface {
	Prepare(context.Context, string, string) error
}

type replayEntry struct {
	digest    [sha256.Size]byte
	done      chan struct{}
	response  []byte
	reserved  int
	readOnly  bool
	expiresAt time.Time
}

type controlRequest struct {
	Protocol       string          `json:"protocol"`
	Type           string          `json:"type"`
	ID             string          `json:"id"`
	Method         string          `json:"method"`
	DeadlineUnixMS int64           `json:"deadlineUnixMs"`
	Params         json.RawMessage `json:"params"`
}

type controlResponse struct {
	Protocol string          `json:"protocol"`
	Type     string          `json:"type"`
	ID       string          `json:"id"`
	OK       bool            `json:"ok"`
	Result   json.RawMessage `json:"result,omitempty"`
	Error    *controlError   `json:"error,omitempty"`
}

type controlError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

type applyCatalogParams struct {
	Catalog json.RawMessage `json:"catalog"`
}

type startIntegrationParams struct {
	IntegrationID string          `json:"integrationId"`
	Config        json.RawMessage `json:"config"`
	Grants        []string        `json:"grants"`
	Workspaces    []Workspace     `json:"workspaces"`
}

type stopIntegrationParams struct {
	IntegrationID string `json:"integrationId"`
	Reason        string `json:"reason"`
}

type callIntegrationParams struct {
	IntegrationID string          `json:"integrationId"`
	Method        string          `json:"method"`
	Params        json.RawMessage `json:"params"`
}

func NewControlServer(policy Policy, manager controlManager, mounters ...workspaceMounter) (*ControlServer, error) {
	if manager == nil {
		return nil, errors.New("control manager is required")
	}
	if len(mounters) > 1 {
		return nil, errors.New("only one workspace mounter may be configured")
	}
	if err := policy.Validate(); err != nil {
		return nil, err
	}
	server := &ControlServer{
		manager: manager, policy: policy, now: time.Now,
		handshakeTimeout: 10 * time.Second,
		replays:          make(map[string]*replayEntry),
	}
	if len(mounters) == 1 {
		server.mounter = mounters[0]
	}
	return server, nil
}

func (server *ControlServer) Handle(serverContext context.Context, data []byte) []byte {
	var request controlRequest
	if err := strictjson.Decode(data, int64(server.policy.MaxMessageBytes), &request); err != nil ||
		request.Protocol != controlProtocol || request.Type != "request" ||
		!requestIDPattern.MatchString(request.ID) || len(request.Params) == 0 || !json.Valid(request.Params) {
		return server.encodeError("invalid", "INVALID_ARGUMENT", "request envelope is invalid")
	}
	now := server.now()
	deadline := time.UnixMilli(request.DeadlineUnixMS)
	if request.DeadlineUnixMS <= 0 || !deadline.After(now) || deadline.After(now.Add(maxRequestLifetime)) {
		return server.encodeError(request.ID, "INVALID_ARGUMENT", "request deadline is invalid")
	}

	digest := sha256.Sum256(data)
	entry, owner, conflict, exhausted := server.acquireReplay(request, digest)
	if conflict {
		return server.encodeError(request.ID, "ALREADY_EXISTS", "request ID was reused with different content")
	}
	if exhausted {
		return server.encodeError(request.ID, "RESOURCE_EXHAUSTED", "replay capacity is unavailable")
	}
	if !owner {
		select {
		case <-entry.done:
			return append([]byte(nil), entry.response...)
		case <-serverContext.Done():
			return server.encodeError(request.ID, "CANCELLED", "request was cancelled")
		}
	}

	response := server.dispatch(serverContext, request)
	if len(response)+1 > server.policy.MaxMessageBytes {
		response = server.encodeError(request.ID, "RESOURCE_EXHAUSTED", "response exceeds the control limit")
	}
	server.completeReplay(request.ID, entry, response)
	return append([]byte(nil), response...)
}

func (server *ControlServer) dispatch(serverContext context.Context, request controlRequest) []byte {
	now := server.now()
	deadline := time.UnixMilli(request.DeadlineUnixMS)
	if request.DeadlineUnixMS <= 0 || !deadline.After(now) || deadline.After(now.Add(maxRequestLifetime)) {
		return server.encodeError(request.ID, "INVALID_ARGUMENT", "request deadline is invalid")
	}
	ctx, cancel := context.WithDeadline(serverContext, deadline)
	defer cancel()

	var result json.RawMessage
	var err error
	switch request.Method {
	case "runner.health":
		if decodeEmptyParams(request.Params) != nil {
			return server.encodeError(request.ID, "INVALID_ARGUMENT", "health parameters are invalid")
		}
		revision, statuses := server.manager.Statuses()
		result, err = json.Marshal(struct {
			ImageVersion    string              `json:"imageVersion"`
			CatalogRevision uint64              `json:"catalogRevision"`
			Integrations    []IntegrationStatus `json:"integrations"`
		}{ImageVersion: server.policy.ImageVersion, CatalogRevision: revision, Integrations: statuses})
	case "catalog.apply":
		var params applyCatalogParams
		if decodeControlParams(request.Params, &params) != nil || len(params.Catalog) == 0 {
			return server.encodeError(request.ID, "INVALID_ARGUMENT", "catalog parameters are invalid")
		}
		err = server.manager.ApplyCatalog(ctx, params.Catalog)
		result = json.RawMessage(`{}`)
	case "integration.start":
		var params startIntegrationParams
		if decodeControlParams(request.Params, &params) != nil || params.IntegrationID == "" ||
			len(params.Config) == 0 || params.Grants == nil || params.Workspaces == nil {
			return server.encodeError(request.ID, "INVALID_ARGUMENT", "start parameters are invalid")
		}
		if validateWorkspaces(server.policy.WorkspaceRoot, params.Workspaces) != nil {
			return server.encodeError(request.ID, "INVALID_ARGUMENT", "workspace parameters are invalid")
		}
		if len(params.Workspaces) > 0 && server.mounter == nil {
			return server.encodeError(request.ID, "FAILED_PRECONDITION", "workspace mounter is unavailable")
		}
		for _, workspace := range params.Workspaces {
			if err = server.mounter.Prepare(ctx, workspace.MountID, workspace.Path); err != nil {
				break
			}
		}
		if err != nil {
			break
		}
		err = server.manager.Start(ctx, params.IntegrationID, params.Config, params.Grants, params.Workspaces)
		result = json.RawMessage(`{}`)
	case "integration.stop":
		var params stopIntegrationParams
		if decodeControlParams(request.Params, &params) != nil || params.IntegrationID == "" || !validShutdownReason(params.Reason) {
			return server.encodeError(request.ID, "INVALID_ARGUMENT", "stop parameters are invalid")
		}
		err = server.manager.Stop(ctx, params.IntegrationID, params.Reason)
		result = json.RawMessage(`{}`)
	case "integration.call":
		var params callIntegrationParams
		if decodeControlParams(request.Params, &params) != nil || params.IntegrationID == "" ||
			(params.Method != "computer-use.observe" && params.Method != "computer-use.act") || len(params.Params) == 0 {
			return server.encodeError(request.ID, "INVALID_ARGUMENT", "call parameters are invalid")
		}
		result, err = server.manager.Call(ctx, params.IntegrationID, params.Method, params.Params)
	default:
		return server.encodeError(request.ID, "UNIMPLEMENTED", "control method is not supported")
	}
	if err != nil {
		if errors.Is(err, context.DeadlineExceeded) || errors.Is(ctx.Err(), context.DeadlineExceeded) {
			return server.encodeError(request.ID, "DEADLINE_EXCEEDED", "request deadline was exceeded")
		}
		if errors.Is(err, context.Canceled) || errors.Is(ctx.Err(), context.Canceled) {
			return server.encodeError(request.ID, "CANCELLED", "request was cancelled")
		}
		return server.encodeError(request.ID, "FAILED_PRECONDITION", "requested operation was rejected")
	}
	if len(result) == 0 || !json.Valid(result) {
		return server.encodeError(request.ID, "INTERNAL", "operation returned an invalid response")
	}
	return encodeControlResponse(controlResponse{Protocol: controlProtocol, Type: "response", ID: request.ID, OK: true, Result: result})
}

func decodeControlParams(data json.RawMessage, destination any) error {
	return strictjson.Decode(data, 16<<20, destination)
}

func decodeEmptyParams(data json.RawMessage) error {
	return strictjson.Decode(data, 128, &struct{}{})
}

func (server *ControlServer) encodeError(id, code, message string) []byte {
	return encodeControlResponse(controlResponse{
		Protocol: controlProtocol, Type: "response", ID: id, OK: false,
		Error: &controlError{Code: code, Message: message},
	})
}

func encodeControlResponse(response controlResponse) []byte {
	data, err := json.Marshal(response)
	if err != nil {
		return nil
	}
	return data
}

func (server *ControlServer) acquireReplay(request controlRequest, digest [sha256.Size]byte) (*replayEntry, bool, bool, bool) {
	server.replayMu.Lock()
	defer server.replayMu.Unlock()
	if existing := server.replays[request.ID]; existing != nil {
		return existing, false, existing.digest != digest, false
	}
	reservation := server.policy.MaxMessageBytes
	server.evictReplayLocked(reservation, server.now())
	if server.replayInflight >= maxReplayInflight || len(server.replays) >= maxReplayEntries ||
		reservation > maxReplayBytes-server.replayBytes {
		return nil, false, false, true
	}
	entry := &replayEntry{
		digest: digest, done: make(chan struct{}), reserved: reservation,
		readOnly: replayRequestIsReadOnly(request), expiresAt: time.UnixMilli(request.DeadlineUnixMS),
	}
	server.replays[request.ID] = entry
	server.order = append(server.order, request.ID)
	server.replayBytes += reservation
	server.replayInflight++
	return entry, true, false, false
}

func (server *ControlServer) completeReplay(id string, entry *replayEntry, response []byte) {
	server.replayMu.Lock()
	server.replayInflight--
	server.replayBytes -= entry.reserved
	entry.response = append([]byte(nil), response...)
	entry.reserved = len(entry.response)
	server.replayBytes += entry.reserved
	close(entry.done)
	server.replayMu.Unlock()
}

func (server *ControlServer) evictReplayLocked(reservation int, now time.Time) {
	for (len(server.replays) >= maxReplayEntries || reservation > maxReplayBytes-server.replayBytes) && len(server.order) > 0 {
		evicted := false
		for index, id := range server.order {
			candidate := server.replays[id]
			if candidate == nil {
				server.order = append(server.order[:index], server.order[index+1:]...)
				evicted = true
				break
			}
			select {
			case <-candidate.done:
				if candidate.readOnly || !candidate.expiresAt.After(now) {
					server.replayBytes -= candidate.reserved
					delete(server.replays, id)
					server.order = append(server.order[:index], server.order[index+1:]...)
					evicted = true
				}
			default:
			}
			if evicted {
				break
			}
		}
		if !evicted {
			return
		}
	}
}

func replayRequestIsReadOnly(request controlRequest) bool {
	switch request.Method {
	case "runner.health":
		return true
	case "catalog.apply", "integration.start", "integration.stop":
		return false
	case "integration.call":
		var params callIntegrationParams
		if decodeControlParams(request.Params, &params) != nil {
			return true
		}
		return params.Method != "computer-use.act"
	default:
		return true
	}
}
