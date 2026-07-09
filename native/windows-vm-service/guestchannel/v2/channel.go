package guestchannelv2

import (
	"context"
	"crypto/hmac"
	"crypto/rand"
	"errors"
	"io"
	"net"
	"sync"
	"time"
)

var (
	ErrReplayServed  = errors.New("authenticated guest request replay was served")
	ErrChannelClosed = errors.New("authenticated guest channel is closed")
)

type BootMaterial struct {
	mu        sync.Mutex
	bootID    [BootIDSize]byte
	key       [ChannelKeySize]byte
	hostNonce [NonceSize]byte
	consumed  bool
}

type bootSnapshot struct {
	bootID    [BootIDSize]byte
	key       [ChannelKeySize]byte
	hostNonce [NonceSize]byte
}

func NewBootMaterial() (*BootMaterial, error) {
	return newBootMaterial(rand.Reader)
}

func newBootMaterial(random io.Reader) (*BootMaterial, error) {
	if random == nil {
		return nil, errProtocol
	}
	material := &BootMaterial{}
	if _, err := io.ReadFull(random, material.bootID[:]); err != nil {
		material.Close()
		return nil, errors.New("guest channel boot identity generation failed")
	}
	if _, err := io.ReadFull(random, material.key[:]); err != nil {
		material.Close()
		return nil, errors.New("guest channel key generation failed")
	}
	if _, err := io.ReadFull(random, material.hostNonce[:]); err != nil {
		material.Close()
		return nil, errors.New("guest channel host nonce generation failed")
	}
	if allBytesZero(material.bootID[:]) || allBytesZero(material.key[:]) || allBytesZero(material.hostNonce[:]) {
		material.Close()
		return nil, errors.New("guest channel random material is invalid")
	}
	return material, nil
}

func (material *BootMaterial) consume() (bootSnapshot, error) {
	if material == nil {
		return bootSnapshot{}, ErrChannelClosed
	}
	material.mu.Lock()
	defer material.mu.Unlock()
	if material.consumed {
		return bootSnapshot{}, ErrChannelClosed
	}
	material.consumed = true
	snapshot := bootSnapshot{bootID: material.bootID, key: material.key, hostNonce: material.hostNonce}
	zero(material.bootID[:])
	zero(material.key[:])
	zero(material.hostNonce[:])
	return snapshot, nil
}

func (material *BootMaterial) Close() {
	if material == nil {
		return
	}
	material.mu.Lock()
	material.consumed = true
	zero(material.bootID[:])
	zero(material.key[:])
	zero(material.hostNonce[:])
	material.mu.Unlock()
}

type HostChannel struct {
	mu                  sync.Mutex
	connection          net.Conn
	interruptConnection net.Conn
	bootID              [BootIDSize]byte
	key                 [ChannelKeySize]byte
	nextOutbound        uint64
	nextInbound         uint64
	maximumFrame        int
	maximumData         int
	poisoned            bool
	now                 func() time.Time
}

func ProvisionHost(ctx context.Context, connection net.Conn, material *BootMaterial, maximumControl int, timeout time.Duration) (*HostChannel, error) {
	if connection == nil || timeout <= 0 {
		return nil, errProtocol
	}
	maximumFrame, err := frameMaximum(maximumControl)
	if err != nil {
		_ = connection.Close()
		return nil, err
	}
	snapshot, err := material.consume()
	if err != nil {
		_ = connection.Close()
		return nil, err
	}
	succeeded := false
	defer func() {
		zero(snapshot.bootID[:])
		zero(snapshot.key[:])
		zero(snapshot.hostNonce[:])
		if !succeeded {
			_ = connection.Close()
		}
	}()
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	interrupt := interruptConnectionOnCancellation(ctx, connection)
	defer interrupt.stopAndWait()
	if err := setOperationDeadline(connection, contextDeadline(ctx), time.Now().Add(timeout)); err != nil {
		return nil, err
	}
	provision := &ProvisionChannel{
		ProtocolVersion: ProtocolVersion,
		BootId:          append([]byte(nil), snapshot.bootID[:]...),
		ChannelKey:      append([]byte(nil), snapshot.key[:]...),
		HostNonce:       append([]byte(nil), snapshot.hostNonce[:]...),
	}
	encoded, err := writeMessage(connection, MaxHandshakeBytes, provision)
	zero(encoded)
	zero(provision.BootId)
	zero(provision.ChannelKey)
	zero(provision.HostNonce)
	if err != nil {
		return nil, err
	}
	ack := &ProvisionChannelAck{}
	encoded, err = readMessage(connection, MaxHandshakeBytes, ack)
	zero(encoded)
	if err != nil {
		return nil, err
	}
	defer func() {
		zero(ack.BootId)
		zero(ack.GuestNonce)
		zero(ack.HmacSha256)
	}()
	if ack.ProtocolVersion != ProtocolVersion || len(ack.BootId) != BootIDSize ||
		!hmac.Equal(ack.BootId, snapshot.bootID[:]) || len(ack.GuestNonce) != NonceSize || allBytesZero(ack.GuestNonce) ||
		len(ack.HmacSha256) != MACSize {
		return nil, errProtocol
	}
	proof := &ProvisionChannelProof{
		ProtocolVersion: ProtocolVersion,
		BootId:          append([]byte(nil), snapshot.bootID[:]...),
		HostNonce:       append([]byte(nil), snapshot.hostNonce[:]...),
		GuestNonce:      append([]byte(nil), ack.GuestNonce...),
	}
	expected, proofErr := proofMAC(&snapshot.key, proof)
	zero(proof.BootId)
	zero(proof.HostNonce)
	zero(proof.GuestNonce)
	if proofErr != nil {
		return nil, proofErr
	}
	defer zero(expected)
	if !hmac.Equal(expected, ack.HmacSha256) {
		return nil, errAuthentication
	}
	if err := connection.SetDeadline(time.Time{}); err != nil {
		return nil, err
	}
	if interrupt.stopAndWait() {
		return nil, ctx.Err()
	}
	channel := &HostChannel{
		connection: connection, interruptConnection: connection,
		bootID: snapshot.bootID, key: snapshot.key,
		nextOutbound: 1, nextInbound: 1, maximumFrame: maximumFrame, maximumData: maximumControl,
		now: time.Now,
	}
	succeeded = true
	return channel, nil
}

func (channel *HostChannel) RoundTrip(ctx context.Context, requestID string, deadline time.Time, controlFrame []byte) ([]byte, error) {
	channel.mu.Lock()
	defer channel.mu.Unlock()
	if channel.poisoned || channel.connection == nil {
		return nil, ErrChannelClosed
	}
	if err := ctx.Err(); err != nil {
		channel.poisonLocked()
		return nil, err
	}
	interrupt := interruptConnectionOnCancellation(ctx, channel.connection)
	defer interrupt.stopAndWait()
	payload := &AuthenticatedPayload{
		ProtocolVersion: ProtocolVersion,
		BootId:          append([]byte(nil), channel.bootID[:]...),
		Direction:       ChannelDirection_CHANNEL_DIRECTION_HOST_TO_GUEST,
		Sequence:        channel.nextOutbound,
		RequestId:       requestID,
		DeadlineUnixMs:  uint64(deadline.UnixMilli()),
		ControlFrame:    append([]byte(nil), controlFrame...),
	}
	defer clearPayload(payload)
	if err := validatePayload(payload, &channel.bootID, ChannelDirection_CHANNEL_DIRECTION_HOST_TO_GUEST, channel.maximumData, channel.now()); err != nil {
		channel.poisonLocked()
		return nil, err
	}
	proof, err := payloadMAC(&channel.key, payload)
	if err != nil {
		channel.poisonLocked()
		return nil, err
	}
	frame := &AuthenticatedFrame{Payload: payload, HmacSha256: proof}
	defer zero(frame.HmacSha256)
	if err := setOperationDeadline(channel.connection, contextDeadline(ctx), deadline); err != nil {
		channel.poisonLocked()
		return nil, operationError(ctx, err)
	}
	encoded, err := writeMessage(channel.connection, channel.maximumFrame, frame)
	zero(encoded)
	if err != nil {
		channel.poisonLocked()
		return nil, operationError(ctx, err)
	}
	response := &AuthenticatedFrame{}
	encoded, err = readMessage(channel.connection, channel.maximumFrame, response)
	zero(encoded)
	if err != nil {
		channel.poisonLocked()
		return nil, operationError(ctx, err)
	}
	defer clearFrame(response)
	if err := verifyPayloadMAC(&channel.key, response); err != nil {
		channel.poisonLocked()
		return nil, err
	}
	if err := validatePayload(response.Payload, &channel.bootID, ChannelDirection_CHANNEL_DIRECTION_GUEST_TO_HOST, channel.maximumData, channel.now()); err != nil ||
		response.Payload.Sequence != channel.nextInbound || response.Payload.RequestId != requestID || response.Payload.DeadlineUnixMs != payload.DeadlineUnixMs {
		channel.poisonLocked()
		return nil, errProtocol
	}
	channel.nextOutbound++
	channel.nextInbound++
	if interrupt.stopAndWait() {
		channel.poisonLocked()
		return nil, ctx.Err()
	}
	_ = channel.connection.SetDeadline(time.Time{})
	return append([]byte(nil), response.Payload.ControlFrame...), nil
}

type cancellationInterrupt struct {
	stop    func() bool
	done    chan struct{}
	stopped bool
}

func interruptConnectionOnCancellation(ctx context.Context, connection net.Conn) *cancellationInterrupt {
	done := make(chan struct{})
	stop := context.AfterFunc(ctx, func() {
		_ = connection.Close()
		close(done)
	})
	return &cancellationInterrupt{stop: stop, done: done}
}

// stopAndWait returns true when cancellation won the race and the connection
// was closed. It is idempotent so callers can use it both on the success path
// and in a defer covering every error path.
func (interrupt *cancellationInterrupt) stopAndWait() bool {
	if interrupt.stopped {
		return false
	}
	interrupt.stopped = true
	if interrupt.stop() {
		return false
	}
	<-interrupt.done
	return true
}

func operationError(ctx context.Context, err error) error {
	if contextErr := ctx.Err(); contextErr != nil {
		return contextErr
	}
	return err
}

func (channel *HostChannel) Close() error {
	if channel == nil {
		return nil
	}
	channel.mu.Lock()
	err := channel.poisonLocked()
	channel.mu.Unlock()
	return err
}

// Abort interrupts an in-flight read or write before taking the channel lock.
// net.Conn permits concurrent Close, and interruptConnection is immutable for
// the lifetime of the channel. Close then performs the normal key zeroization.
func (channel *HostChannel) Abort() error {
	if channel == nil {
		return nil
	}
	_ = channel.interruptConnection.Close()
	return channel.Close()
}

func (channel *HostChannel) Poisoned() bool {
	if channel == nil {
		return true
	}
	channel.mu.Lock()
	defer channel.mu.Unlock()
	return channel.poisoned
}

func (channel *HostChannel) poisonLocked() error {
	if channel.poisoned {
		return nil
	}
	channel.poisoned = true
	zero(channel.key[:])
	zero(channel.bootID[:])
	if channel.connection == nil {
		return nil
	}
	err := channel.connection.Close()
	channel.connection = nil
	return err
}

type GuestRequest struct {
	payload   *AuthenticatedPayload
	canonical []byte
}

func (request *GuestRequest) RequestID() string {
	if request == nil || request.payload == nil {
		return ""
	}
	return request.payload.RequestId
}

func (request *GuestRequest) DeadlineUnixMS() uint64 {
	if request == nil || request.payload == nil {
		return 0
	}
	return request.payload.DeadlineUnixMs
}

func (request *GuestRequest) ControlFrame() []byte {
	if request == nil || request.payload == nil {
		return nil
	}
	return append([]byte(nil), request.payload.ControlFrame...)
}

type replayRecord struct {
	requestID string
	request   []byte
	response  []byte
	deadline  time.Time
}

type GuestChannel struct {
	mu           sync.Mutex
	connection   net.Conn
	bootID       [BootIDSize]byte
	key          [ChannelKeySize]byte
	nextInbound  uint64
	nextOutbound uint64
	maximumFrame int
	maximumData  int
	poisoned     bool
	now          func() time.Time
	pending      *GuestRequest
	replays      map[uint64]*replayRecord
	requestIDs   map[string]uint64
	replayBytes  int
}

func AcceptGuest(ctx context.Context, connection net.Conn, maximumControl int, timeout time.Duration) (*GuestChannel, error) {
	return acceptGuest(ctx, connection, maximumControl, timeout, rand.Reader)
}

func acceptGuest(ctx context.Context, connection net.Conn, maximumControl int, timeout time.Duration, random io.Reader) (*GuestChannel, error) {
	if connection == nil || random == nil || timeout <= 0 {
		return nil, errProtocol
	}
	maximumFrame, err := frameMaximum(maximumControl)
	if err != nil {
		_ = connection.Close()
		return nil, err
	}
	succeeded := false
	defer func() {
		if !succeeded {
			_ = connection.Close()
		}
	}()
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if err := setOperationDeadline(connection, contextDeadline(ctx), time.Now().Add(timeout)); err != nil {
		return nil, err
	}
	provision := &ProvisionChannel{}
	encoded, err := readMessage(connection, MaxHandshakeBytes, provision)
	zero(encoded)
	if err != nil {
		return nil, err
	}
	defer func() {
		zero(provision.BootId)
		zero(provision.ChannelKey)
		zero(provision.HostNonce)
	}()
	if provision.ProtocolVersion != ProtocolVersion || len(provision.BootId) != BootIDSize ||
		len(provision.ChannelKey) != ChannelKeySize || len(provision.HostNonce) != NonceSize ||
		allBytesZero(provision.BootId) || allBytesZero(provision.ChannelKey) || allBytesZero(provision.HostNonce) {
		return nil, errProtocol
	}
	channel := &GuestChannel{
		connection: connection, nextInbound: 1, nextOutbound: 1,
		maximumFrame: maximumFrame, maximumData: maximumControl, now: time.Now,
		replays: make(map[uint64]*replayRecord), requestIDs: make(map[string]uint64),
	}
	copy(channel.bootID[:], provision.BootId)
	copy(channel.key[:], provision.ChannelKey)
	guestNonce := make([]byte, NonceSize)
	defer zero(guestNonce)
	if _, err := io.ReadFull(random, guestNonce); err != nil {
		channel.poisonLocked()
		return nil, errors.New("guest channel nonce generation failed")
	}
	if allBytesZero(guestNonce) {
		channel.poisonLocked()
		return nil, errors.New("guest channel nonce is invalid")
	}
	proof := &ProvisionChannelProof{
		ProtocolVersion: ProtocolVersion,
		BootId:          append([]byte(nil), channel.bootID[:]...),
		HostNonce:       append([]byte(nil), provision.HostNonce...),
		GuestNonce:      append([]byte(nil), guestNonce...),
	}
	authentication, err := proofMAC(&channel.key, proof)
	zero(proof.BootId)
	zero(proof.HostNonce)
	zero(proof.GuestNonce)
	if err != nil {
		channel.poisonLocked()
		return nil, err
	}
	ack := &ProvisionChannelAck{
		ProtocolVersion: ProtocolVersion,
		BootId:          append([]byte(nil), channel.bootID[:]...),
		GuestNonce:      append([]byte(nil), guestNonce...),
		HmacSha256:      authentication,
	}
	encoded, err = writeMessage(connection, MaxHandshakeBytes, ack)
	zero(encoded)
	zero(ack.BootId)
	zero(ack.GuestNonce)
	zero(ack.HmacSha256)
	if err != nil {
		channel.poisonLocked()
		return nil, err
	}
	if err := connection.SetDeadline(time.Time{}); err != nil {
		channel.poisonLocked()
		return nil, err
	}
	succeeded = true
	return channel, nil
}

func (channel *GuestChannel) Receive(ctx context.Context) (*GuestRequest, error) {
	channel.mu.Lock()
	defer channel.mu.Unlock()
	if channel.poisoned || channel.connection == nil {
		return nil, ErrChannelClosed
	}
	if channel.pending != nil {
		channel.poisonLocked()
		return nil, errProtocol
	}
	if err := ctx.Err(); err != nil {
		channel.poisonLocked()
		return nil, err
	}
	if err := setOperationDeadline(channel.connection, contextDeadline(ctx), time.Time{}); err != nil {
		channel.poisonLocked()
		return nil, err
	}
	frame := &AuthenticatedFrame{}
	canonical, err := readMessage(channel.connection, channel.maximumFrame, frame)
	if err != nil {
		channel.poisonLocked()
		return nil, err
	}
	if err := verifyPayloadMAC(&channel.key, frame); err != nil {
		zero(canonical)
		clearFrame(frame)
		channel.poisonLocked()
		return nil, err
	}
	now := channel.now()
	if err := validatePayload(frame.Payload, &channel.bootID, ChannelDirection_CHANNEL_DIRECTION_HOST_TO_GUEST, channel.maximumData, now); err != nil {
		zero(canonical)
		clearFrame(frame)
		channel.poisonLocked()
		return nil, err
	}
	channel.evictExpiredLocked(now)
	sequence := frame.Payload.Sequence
	if sequence < channel.nextInbound {
		replay := channel.replays[sequence]
		if replay == nil || !hmac.Equal(replay.request, canonical) {
			zero(canonical)
			clearFrame(frame)
			channel.poisonLocked()
			return nil, errProtocol
		}
		zero(canonical)
		clearFrame(frame)
		if err := writeCanonical(channel.connection, channel.maximumFrame, replay.response); err != nil {
			channel.poisonLocked()
			return nil, err
		}
		return nil, ErrReplayServed
	}
	if sequence != channel.nextInbound || len(channel.replays) >= MaxReplayEntries {
		zero(canonical)
		clearFrame(frame)
		channel.poisonLocked()
		return nil, errProtocol
	}
	if previous, exists := channel.requestIDs[frame.Payload.RequestId]; exists && previous != sequence {
		zero(canonical)
		clearFrame(frame)
		channel.poisonLocked()
		return nil, errProtocol
	}
	request := &GuestRequest{payload: frame.Payload, canonical: canonical}
	frame.Payload = nil
	zero(frame.HmacSha256)
	channel.pending = request
	channel.nextInbound++
	return request, nil
}

func (channel *GuestChannel) Respond(ctx context.Context, request *GuestRequest, controlFrame []byte) error {
	channel.mu.Lock()
	defer channel.mu.Unlock()
	if channel.poisoned || channel.connection == nil {
		return ErrChannelClosed
	}
	if request == nil || request != channel.pending || request.payload == nil {
		channel.poisonLocked()
		return errProtocol
	}
	if err := ctx.Err(); err != nil {
		channel.poisonLocked()
		return err
	}
	payload := &AuthenticatedPayload{
		ProtocolVersion: ProtocolVersion,
		BootId:          append([]byte(nil), channel.bootID[:]...),
		Direction:       ChannelDirection_CHANNEL_DIRECTION_GUEST_TO_HOST,
		Sequence:        channel.nextOutbound,
		RequestId:       request.payload.RequestId,
		DeadlineUnixMs:  request.payload.DeadlineUnixMs,
		ControlFrame:    append([]byte(nil), controlFrame...),
	}
	defer clearPayload(payload)
	now := channel.now()
	if err := validatePayload(payload, &channel.bootID, ChannelDirection_CHANNEL_DIRECTION_GUEST_TO_HOST, channel.maximumData, now); err != nil {
		channel.poisonLocked()
		return err
	}
	authentication, err := payloadMAC(&channel.key, payload)
	if err != nil {
		channel.poisonLocked()
		return err
	}
	frame := &AuthenticatedFrame{Payload: payload, HmacSha256: authentication}
	response, err := marshalOptions.Marshal(frame)
	zero(frame.HmacSha256)
	if err != nil || len(response) == 0 || len(response) > channel.maximumFrame {
		zero(response)
		channel.poisonLocked()
		return errProtocol
	}
	deadline := time.UnixMilli(int64(request.payload.DeadlineUnixMs))
	channel.evictExpiredLocked(now)
	entryBytes := len(request.canonical) + len(response)
	if len(channel.replays) >= MaxReplayEntries || entryBytes > MaxReplayBytes-channel.replayBytes {
		zero(response)
		channel.poisonLocked()
		return errProtocol
	}
	sequence := request.payload.Sequence
	record := &replayRecord{
		requestID: request.payload.RequestId,
		request:   request.canonical, response: response, deadline: deadline,
	}
	channel.replays[sequence] = record
	channel.requestIDs[record.requestID] = sequence
	channel.replayBytes += entryBytes
	request.canonical = nil
	if err := setOperationDeadline(channel.connection, contextDeadline(ctx), deadline); err != nil {
		channel.poisonLocked()
		return err
	}
	if err := writeCanonical(channel.connection, channel.maximumFrame, response); err != nil {
		channel.poisonLocked()
		return err
	}
	channel.nextOutbound++
	clearPayload(request.payload)
	request.payload = nil
	channel.pending = nil
	_ = channel.connection.SetDeadline(time.Time{})
	return nil
}

func (channel *GuestChannel) Close() error {
	if channel == nil {
		return nil
	}
	channel.mu.Lock()
	err := channel.poisonLocked()
	channel.mu.Unlock()
	return err
}

func (channel *GuestChannel) Poisoned() bool {
	if channel == nil {
		return true
	}
	channel.mu.Lock()
	defer channel.mu.Unlock()
	return channel.poisoned
}

func (channel *GuestChannel) evictExpiredLocked(now time.Time) {
	for sequence, record := range channel.replays {
		if record.deadline.After(now) {
			continue
		}
		channel.replayBytes -= len(record.request) + len(record.response)
		delete(channel.requestIDs, record.requestID)
		zero(record.request)
		zero(record.response)
		delete(channel.replays, sequence)
	}
}

func (channel *GuestChannel) poisonLocked() error {
	if channel.poisoned {
		return nil
	}
	channel.poisoned = true
	zero(channel.key[:])
	zero(channel.bootID[:])
	if channel.pending != nil {
		clearPayload(channel.pending.payload)
		zero(channel.pending.canonical)
		channel.pending = nil
	}
	for sequence, record := range channel.replays {
		zero(record.request)
		zero(record.response)
		delete(channel.replays, sequence)
	}
	clear(channel.requestIDs)
	channel.replayBytes = 0
	if channel.connection == nil {
		return nil
	}
	err := channel.connection.Close()
	channel.connection = nil
	return err
}

func clearFrame(frame *AuthenticatedFrame) {
	if frame == nil {
		return
	}
	clearPayload(frame.Payload)
	zero(frame.HmacSha256)
	frame.Payload = nil
}

func clearPayload(payload *AuthenticatedPayload) {
	if payload == nil {
		return
	}
	zero(payload.BootId)
	zero(payload.ControlFrame)
	payload.BootId = nil
	payload.ControlFrame = nil
}

func allBytesZero(data []byte) bool {
	var combined byte
	for _, value := range data {
		combined |= value
	}
	return combined == 0
}
