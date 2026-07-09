package vmservice

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"net"
	"regexp"
	"sync"
	"time"

	guestchannelv2 "github.com/grok-insider/grok-desktop/native/windows-vm-service/guestchannel/v2"
)

const (
	// The named-pipe transport is capped at 8 MiB. Keep enough room for its
	// request/response envelope without base64-expanding the nested JSON.
	DefaultGuestControlMaxBytes = (8 << 20) - (64 << 10)
	guestControlProtocol        = "grok.guest-control/v1"
	guestChannelHandshake       = 10 * time.Second
)

var (
	errGuestChannelRetired = errors.New("guest channel ownership was retired")
	guestOperationID       = regexp.MustCompile(`^[A-Za-z0-9._:-]{16,128}$`)
	guestErrorCode         = regexp.MustCompile(`^[A-Z][A-Z0-9_]{0,63}$`)
)

type guestSocketDialer interface {
	Dial(context.Context, string, SocketPurpose) (net.Conn, error)
}

type guestChannelKey struct {
	vmID      string
	runtimeID string
	purpose   SocketPurpose
}

type guestChannelEntry struct {
	mu      sync.Mutex
	retired bool
	channel *guestchannelv2.HostChannel
}

type guestChannelPool struct {
	mu               sync.Mutex
	dialer           guestSocketDialer
	maximumControl   int
	handshakeTimeout time.Duration
	entries          map[guestChannelKey]*guestChannelEntry
	closed           bool
}

func newGuestChannelPool(dialer guestSocketDialer, maximumControl int) *guestChannelPool {
	return &guestChannelPool{
		dialer: dialer, maximumControl: maximumControl,
		handshakeTimeout: guestChannelHandshake,
		entries:          make(map[guestChannelKey]*guestChannelEntry),
	}
}

func (pool *guestChannelPool) roundTrip(
	ctx context.Context,
	key guestChannelKey,
	requestID string,
	deadline time.Time,
	controlFrame []byte,
) ([]byte, error) {
	entry, err := pool.entry(key)
	if err != nil {
		return nil, err
	}
	channel, err := pool.channel(ctx, entry, key)
	if err != nil {
		return nil, err
	}
	response, err := channel.RoundTrip(ctx, requestID, deadline, controlFrame)
	if err != nil {
		entry.discard(channel)
		return nil, err
	}
	return response, nil
}

func (pool *guestChannelPool) ensure(ctx context.Context, key guestChannelKey) error {
	entry, err := pool.entry(key)
	if err != nil {
		return err
	}
	_, err = pool.channel(ctx, entry, key)
	return err
}

func (pool *guestChannelPool) entry(key guestChannelKey) (*guestChannelEntry, error) {
	pool.mu.Lock()
	defer pool.mu.Unlock()
	if pool.closed {
		return nil, errGuestChannelRetired
	}
	entry := pool.entries[key]
	if entry == nil {
		entry = &guestChannelEntry{}
		pool.entries[key] = entry
	}
	return entry, nil
}

func (pool *guestChannelPool) channel(
	ctx context.Context,
	entry *guestChannelEntry,
	key guestChannelKey,
) (*guestchannelv2.HostChannel, error) {
	entry.mu.Lock()
	defer entry.mu.Unlock()
	if entry.retired {
		return nil, errGuestChannelRetired
	}
	if entry.channel != nil && !entry.channel.Poisoned() {
		return entry.channel, nil
	}
	connection, err := pool.dialer.Dial(ctx, key.runtimeID, key.purpose)
	if err != nil {
		return nil, err
	}
	material, err := guestchannelv2.NewBootMaterial()
	if err != nil {
		_ = connection.Close()
		return nil, err
	}
	defer material.Close()
	channel, err := guestchannelv2.ProvisionHost(
		ctx,
		connection,
		material,
		pool.maximumControl,
		pool.handshakeTimeout,
	)
	if err != nil {
		return nil, err
	}
	entry.channel = channel
	return channel, nil
}

func (pool *guestChannelPool) closeVM(vmID string) {
	pool.mu.Lock()
	entries := make([]*guestChannelEntry, 0, len(pool.entries))
	for key, entry := range pool.entries {
		if key.vmID == vmID {
			delete(pool.entries, key)
			entries = append(entries, entry)
		}
	}
	pool.mu.Unlock()
	for _, entry := range entries {
		entry.retire()
	}
}

func (pool *guestChannelPool) closeKey(key guestChannelKey) {
	pool.mu.Lock()
	entry := pool.entries[key]
	if entry != nil {
		delete(pool.entries, key)
	}
	pool.mu.Unlock()
	if entry != nil {
		entry.retire()
	}
}

func (pool *guestChannelPool) close() {
	pool.mu.Lock()
	if pool.closed {
		pool.mu.Unlock()
		return
	}
	pool.closed = true
	entries := make([]*guestChannelEntry, 0, len(pool.entries))
	for key, entry := range pool.entries {
		delete(pool.entries, key)
		entries = append(entries, entry)
	}
	pool.mu.Unlock()
	for _, entry := range entries {
		entry.retire()
	}
}

func (entry *guestChannelEntry) discard(channel *guestchannelv2.HostChannel) {
	entry.mu.Lock()
	if entry.channel == channel {
		entry.channel = nil
	}
	entry.mu.Unlock()
	_ = channel.Close()
}

func (entry *guestChannelEntry) retire() {
	entry.mu.Lock()
	entry.retired = true
	channel := entry.channel
	entry.channel = nil
	entry.mu.Unlock()
	_ = channel.Abort()
}

type guestControlWireRequest struct {
	Protocol       string          `json:"protocol"`
	Type           string          `json:"type"`
	ID             string          `json:"id"`
	Method         string          `json:"method"`
	DeadlineUnixMS int64           `json:"deadlineUnixMs"`
	Params         json.RawMessage `json:"params"`
}

type guestControlWireResponse struct {
	Protocol string                 `json:"protocol"`
	Type     string                 `json:"type"`
	ID       string                 `json:"id"`
	OK       bool                   `json:"ok"`
	Result   json.RawMessage        `json:"result,omitempty"`
	Error    *guestControlWireError `json:"error,omitempty"`
}

type guestControlWireError struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

func (s *hcsService) GuestControl(ctx context.Context, request GuestControlRequest) (GuestControlResult, error) {
	if err := s.begin(ctx, request.Request); err != nil {
		return GuestControlResult{}, err
	}
	if err := validateID("vmId", request.VmID); err != nil {
		return GuestControlResult{}, err
	}
	if !guestOperationID.MatchString(request.OperationID) {
		return GuestControlResult{}, serviceError(CodeInvalidArgument, "operationId must contain 16 to 128 safe characters")
	}
	if err := validateGuestControlMethod(request.Method); err != nil {
		return GuestControlResult{}, err
	}
	params := bytes.TrimSpace(request.Params)
	if len(params) == 0 || len(params) > s.config.guestControlMaxBytes || params[0] != '{' || !json.Valid(params) {
		return GuestControlResult{}, serviceError(CodeInvalidArgument, "guest control parameters must be a bounded JSON object")
	}
	deadline, present := ctx.Deadline()
	deadline = time.UnixMilli(deadline.UnixMilli())
	now := s.now()
	if !present || !deadline.After(now) || deadline.After(now.Add(guestchannelv2.MaxRequestLifetime)) {
		return GuestControlResult{}, serviceError(CodeInvalidArgument, "guest control requires a valid bounded deadline")
	}

	s.mu.Lock()
	vm, ok := s.state.VMs[request.VmID]
	if !ok || vm.Deleting {
		s.mu.Unlock()
		return GuestControlResult{}, serviceError(CodeNotFound, "VM %q does not exist", request.VmID)
	}
	if vm.State != VmStateRunning || !guidPattern.MatchString(vm.RuntimeID) {
		s.mu.Unlock()
		return GuestControlResult{}, serviceError(CodeConflict, "VM %q must be running before guest control", vm.ID)
	}
	if _, allowed := s.config.allowedSocketPurposes[SocketPurposeControl]; !allowed {
		s.mu.Unlock()
		return GuestControlResult{}, serviceError(CodePermissionDenied, "guest control socket purpose is disabled")
	}
	runtimeID := vm.RuntimeID
	s.mu.Unlock()

	controlFrame, err := json.Marshal(guestControlWireRequest{
		Protocol: guestControlProtocol, Type: "request", ID: request.OperationID,
		Method: string(request.Method), DeadlineUnixMS: deadline.UnixMilli(),
		Params: append(json.RawMessage(nil), params...),
	})
	if err != nil || len(controlFrame) > s.config.guestControlMaxBytes {
		return GuestControlResult{}, serviceError(CodeInvalidArgument, "guest control request exceeds its size limit")
	}
	key := guestChannelKey{
		vmID: request.VmID, runtimeID: runtimeID, purpose: SocketPurposeControl,
	}
	response, err := s.channels.roundTrip(ctx, key, request.OperationID, deadline, controlFrame)
	zeroGuestControl(controlFrame)
	if err != nil {
		return GuestControlResult{}, guestChannelServiceError(ctx, err)
	}
	if err := validateGuestControlResponse(response, request.OperationID, s.config.guestControlMaxBytes); err != nil {
		s.channels.closeKey(key)
		zeroGuestControl(response)
		return GuestControlResult{}, err
	}
	return GuestControlResult{Response: json.RawMessage(response)}, nil
}

func validateGuestControlResponse(data []byte, operationID string, maximum int) error {
	if len(data) == 0 || len(data) > maximum || data[0] != '{' || !json.Valid(data) {
		return serviceError(CodeUnavailable, "guest control returned an invalid response")
	}
	var response guestControlWireResponse
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&response); err != nil || ensureJSONEOF(decoder) != nil ||
		response.Protocol != guestControlProtocol || response.Type != "response" || response.ID != operationID {
		return serviceError(CodeUnavailable, "guest control returned an invalid response")
	}
	if response.OK {
		if response.Error != nil || len(response.Result) == 0 || !json.Valid(response.Result) {
			return serviceError(CodeUnavailable, "guest control returned an invalid response")
		}
		return nil
	}
	if len(response.Result) != 0 || response.Error == nil || !guestErrorCode.MatchString(response.Error.Code) ||
		response.Error.Message == "" || len(response.Error.Message) > 1024 {
		return serviceError(CodeUnavailable, "guest control returned an invalid response")
	}
	return nil
}

func guestChannelServiceError(ctx context.Context, err error) error {
	if ctx.Err() != nil {
		return contextErrorMessage(ctx.Err())
	}
	return serviceError(CodeUnavailable, "authenticated guest control channel is unavailable")
}

func zeroGuestControl(data []byte) {
	for index := range data {
		data[index] = 0
	}
}
