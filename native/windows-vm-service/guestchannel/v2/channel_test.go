package guestchannelv2

import (
	"bytes"
	"context"
	"errors"
	"net"
	"testing"
	"time"
)

const testMaximumControl = 4096

func TestAuthenticatedRoundTripAndCloseZeroizeKeys(t *testing.T) {
	host, guest := newChannelPair(t, testMaximumControl)
	guestResult := make(chan error, 1)
	go func() {
		request, err := guest.Receive(context.Background())
		if err != nil {
			guestResult <- err
			return
		}
		if request.RequestID() != "request-1" || string(request.ControlFrame()) != `{"type":"request"}` {
			guestResult <- errors.New("guest received unexpected authenticated request")
			return
		}
		guestResult <- guest.Respond(context.Background(), request, []byte(`{"type":"response"}`))
	}()
	response, err := host.RoundTrip(
		context.Background(),
		"request-1",
		time.Now().Add(time.Minute),
		[]byte(`{"type":"request"}`),
	)
	if err != nil {
		t.Fatalf("RoundTrip: %v", err)
	}
	if err := <-guestResult; err != nil {
		t.Fatalf("guest response: %v", err)
	}
	if string(response) != `{"type":"response"}` {
		t.Fatalf("response = %q", response)
	}
	if err := host.Close(); err != nil {
		t.Fatalf("host Close: %v", err)
	}
	if err := guest.Close(); err != nil {
		t.Fatalf("guest Close: %v", err)
	}
	if !allZero(host.key[:]) || !allZero(guest.key[:]) || !host.Poisoned() || !guest.Poisoned() {
		t.Fatal("channel close did not poison and zero both channel keys")
	}
}

func TestRoundTripCancellationInterruptsBlockedConnection(t *testing.T) {
	host, guest := newChannelPair(t, testMaximumControl)
	guestReceived := make(chan error, 1)
	go func() {
		_, err := guest.Receive(context.Background())
		guestReceived <- err
	}()
	ctx, cancel := context.WithCancel(context.Background())
	result := make(chan error, 1)
	go func() {
		_, err := host.RoundTrip(
			ctx,
			"request-cancel",
			time.Now().Add(time.Minute),
			[]byte(`{"type":"request"}`),
		)
		result <- err
	}()
	select {
	case err := <-guestReceived:
		if err != nil {
			t.Fatalf("guest did not receive request: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("guest did not receive request")
	}
	cancel()
	select {
	case err := <-result:
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("RoundTrip error = %v, want context cancellation", err)
		}
	case <-time.After(time.Second):
		t.Fatal("cancellation did not interrupt the blocked guest connection")
	}
	if !host.Poisoned() {
		t.Fatal("cancelled round trip did not poison the host channel")
	}
}

func TestAbortInterruptsBlockedRoundTripAndZeroizesKeys(t *testing.T) {
	host, guest := newChannelPair(t, testMaximumControl)
	guestReceived := make(chan error, 1)
	go func() {
		_, err := guest.Receive(context.Background())
		guestReceived <- err
	}()
	result := make(chan error, 1)
	go func() {
		_, err := host.RoundTrip(
			context.Background(),
			"request-abort",
			time.Now().Add(time.Minute),
			[]byte(`{"type":"request"}`),
		)
		result <- err
	}()
	select {
	case err := <-guestReceived:
		if err != nil {
			t.Fatalf("guest did not receive request: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("guest did not receive request")
	}
	aborted := make(chan error, 1)
	go func() { aborted <- host.Abort() }()
	select {
	case <-result:
	case <-time.After(time.Second):
		t.Fatal("Abort did not interrupt the blocked round trip")
	}
	select {
	case err := <-aborted:
		if err != nil && !errors.Is(err, net.ErrClosed) {
			t.Fatalf("Abort: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Abort did not finish zeroizing the channel")
	}
	if !host.Poisoned() || !allZero(host.key[:]) {
		t.Fatal("Abort did not poison and zero the host channel")
	}
}

func TestGuestRejectsTamperGapStaleBootDirectionDeadlineAndOversize(t *testing.T) {
	tests := []struct {
		name   string
		mutate func(*AuthenticatedPayload, *HostChannel)
		tamper bool
	}{
		{name: "tampered control bytes", tamper: true},
		{name: "sequence gap", mutate: func(payload *AuthenticatedPayload, _ *HostChannel) { payload.Sequence = 2 }},
		{name: "stale boot", mutate: func(payload *AuthenticatedPayload, _ *HostChannel) { payload.BootId[0] ^= 0xff }},
		{name: "wrong direction", mutate: func(payload *AuthenticatedPayload, _ *HostChannel) {
			payload.Direction = ChannelDirection_CHANNEL_DIRECTION_GUEST_TO_HOST
		}},
		{name: "expired deadline", mutate: func(payload *AuthenticatedPayload, _ *HostChannel) {
			payload.DeadlineUnixMs = uint64(time.Now().Add(-time.Second).UnixMilli())
		}},
		{name: "oversized control", mutate: func(payload *AuthenticatedPayload, host *HostChannel) {
			payload.ControlFrame = bytes.Repeat([]byte{'x'}, host.maximumData+1)
		}},
		{name: "invalid request id", mutate: func(payload *AuthenticatedPayload, _ *HostChannel) {
			payload.RequestId = "invalid request id"
		}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			host, guest := newChannelPair(t, testMaximumControl)
			payload := validHostPayload(host, 1, "request-1", []byte(`{"method":"catalog.apply"}`))
			if test.mutate != nil {
				test.mutate(payload, host)
			}
			frame := authenticatedFrame(t, &host.key, payload)
			if test.tamper {
				frame.Payload.ControlFrame[0] ^= 1
			}
			writeDone := make(chan error, 1)
			go func() {
				encoded, err := writeMessage(host.connection, host.maximumFrame, frame)
				zero(encoded)
				writeDone <- err
			}()
			if _, err := guest.Receive(context.Background()); err == nil {
				t.Fatal("invalid authenticated frame was accepted")
			}
			<-writeDone
			if !guest.Poisoned() {
				t.Fatal("protocol violation did not poison the guest channel")
			}
			clearFrame(frame)
			_ = host.Close()
		})
	}
}

func TestGuestReplaysOnlyIdenticalAuthenticatedBytes(t *testing.T) {
	host, guest := newChannelPair(t, testMaximumControl)
	frame := authenticatedFrame(t, &host.key, validHostPayload(host, 1, "request-1", []byte(`{"method":"runner.health"}`)))
	requestCanonical := sendFrameAndReceive(t, host, guest, frame)

	responseRead := make(chan struct {
		canonical []byte
		err       error
	}, 1)
	go func() {
		response := &AuthenticatedFrame{}
		canonical, err := readMessage(host.connection, host.maximumFrame, response)
		clearFrame(response)
		responseRead <- struct {
			canonical []byte
			err       error
		}{canonical: canonical, err: err}
	}()
	request := guest.pending
	if err := guest.Respond(context.Background(), request, []byte(`{"ok":true}`)); err != nil {
		t.Fatalf("Respond: %v", err)
	}
	firstResponse := <-responseRead
	if firstResponse.err != nil {
		t.Fatalf("read first response: %v", firstResponse.err)
	}

	replayResult := make(chan error, 1)
	go func() {
		_, err := guest.Receive(context.Background())
		replayResult <- err
	}()
	if err := writeCanonical(host.connection, host.maximumFrame, requestCanonical); err != nil {
		t.Fatalf("write replay: %v", err)
	}
	replayed := &AuthenticatedFrame{}
	replayedCanonical, err := readMessage(host.connection, host.maximumFrame, replayed)
	clearFrame(replayed)
	if err != nil {
		t.Fatalf("read replay response: %v", err)
	}
	if err := <-replayResult; !errors.Is(err, ErrReplayServed) {
		t.Fatalf("replay result = %v", err)
	}
	if !bytes.Equal(firstResponse.canonical, replayedCanonical) {
		t.Fatal("identical request replay did not return byte-identical response")
	}
	zero(firstResponse.canonical)
	zero(replayedCanonical)
	zero(requestCanonical)
	clearFrame(frame)
	_ = host.Close()
	_ = guest.Close()
}

func TestGuestRejectsSequenceReuseWithDifferentAuthenticatedBytes(t *testing.T) {
	host, guest := newChannelPair(t, testMaximumControl)
	first := authenticatedFrame(t, &host.key, validHostPayload(host, 1, "request-1", []byte(`{"value":1}`)))
	requestCanonical := sendFrameAndReceive(t, host, guest, first)
	readDone := make(chan error, 1)
	go func() {
		response := &AuthenticatedFrame{}
		encoded, err := readMessage(host.connection, host.maximumFrame, response)
		zero(encoded)
		clearFrame(response)
		readDone <- err
	}()
	if err := guest.Respond(context.Background(), guest.pending, []byte(`{"ok":true}`)); err != nil {
		t.Fatalf("Respond: %v", err)
	}
	if err := <-readDone; err != nil {
		t.Fatalf("read response: %v", err)
	}

	conflict := authenticatedFrame(t, &host.key, validHostPayload(host, 1, "request-1", []byte(`{"value":2}`)))
	writeDone := make(chan error, 1)
	go func() {
		encoded, err := writeMessage(host.connection, host.maximumFrame, conflict)
		zero(encoded)
		writeDone <- err
	}()
	if _, err := guest.Receive(context.Background()); err == nil {
		t.Fatal("conflicting sequence replay was accepted")
	}
	<-writeDone
	if !guest.Poisoned() {
		t.Fatal("conflicting sequence replay did not poison the channel")
	}
	zero(requestCanonical)
	clearFrame(first)
	clearFrame(conflict)
	_ = host.Close()
}

func TestGuestPoisonsChannelWhenReplayCapacityCannotRetainResponse(t *testing.T) {
	host, guest := newChannelPair(t, testMaximumControl)
	frame := authenticatedFrame(t, &host.key, validHostPayload(host, 1, "request-1", []byte(`{"method":"integration.call"}`)))
	canonical := sendFrameAndReceive(t, host, guest, frame)
	zero(canonical)
	guest.mu.Lock()
	guest.replayBytes = MaxReplayBytes
	guest.mu.Unlock()
	if err := guest.Respond(context.Background(), guest.pending, []byte(`{"ok":true}`)); err == nil {
		t.Fatal("response was returned without bounded replay capacity")
	}
	if !guest.Poisoned() {
		t.Fatal("replay capacity overflow did not poison the channel")
	}
	clearFrame(frame)
	_ = host.Close()
}

func TestProvisioningRejectsInvalidAckProofAndTimesOut(t *testing.T) {
	t.Run("invalid proof", func(t *testing.T) {
		hostConnection, guestConnection := net.Pipe()
		material, err := NewBootMaterial()
		if err != nil {
			t.Fatal(err)
		}
		result := make(chan error, 1)
		go func() {
			_, err := ProvisionHost(context.Background(), hostConnection, material, testMaximumControl, time.Second)
			result <- err
		}()
		provision := &ProvisionChannel{}
		encoded, err := readMessage(guestConnection, MaxHandshakeBytes, provision)
		zero(encoded)
		if err != nil {
			t.Fatalf("read provision: %v", err)
		}
		ack := &ProvisionChannelAck{
			ProtocolVersion: ProtocolVersion,
			BootId:          append([]byte(nil), provision.BootId...),
			GuestNonce:      bytes.Repeat([]byte{7}, NonceSize),
			HmacSha256:      bytes.Repeat([]byte{0}, MACSize),
		}
		encoded, err = writeMessage(guestConnection, MaxHandshakeBytes, ack)
		zero(encoded)
		if err != nil {
			t.Fatalf("write ack: %v", err)
		}
		if err := <-result; !errors.Is(err, errAuthentication) {
			t.Fatalf("ProvisionHost error = %v", err)
		}
		if !allZero(material.key[:]) || !material.consumed {
			t.Fatal("failed provisioning did not consume and zero boot material")
		}
		clearProvision(provision)
		zero(ack.BootId)
		zero(ack.GuestNonce)
		zero(ack.HmacSha256)
		_ = guestConnection.Close()
	})

	t.Run("guest timeout", func(t *testing.T) {
		hostConnection, guestConnection := net.Pipe()
		started := time.Now()
		_, err := AcceptGuest(context.Background(), guestConnection, testMaximumControl, 20*time.Millisecond)
		if err == nil {
			t.Fatal("guest provisioning did not time out")
		}
		if elapsed := time.Since(started); elapsed > time.Second {
			t.Fatalf("guest provisioning timeout took %s", elapsed)
		}
		_ = hostConnection.Close()
	})

	t.Run("all-zero material", func(t *testing.T) {
		hostConnection, guestConnection := net.Pipe()
		result := make(chan error, 1)
		go func() {
			_, err := AcceptGuest(context.Background(), guestConnection, testMaximumControl, time.Second)
			result <- err
		}()
		provision := &ProvisionChannel{
			ProtocolVersion: ProtocolVersion,
			BootId:          make([]byte, BootIDSize),
			ChannelKey:      make([]byte, ChannelKeySize),
			HostNonce:       make([]byte, NonceSize),
		}
		encoded, err := writeMessage(hostConnection, MaxHandshakeBytes, provision)
		zero(encoded)
		if err != nil {
			t.Fatalf("write provision: %v", err)
		}
		select {
		case err := <-result:
			if err == nil {
				t.Fatal("guest accepted all-zero provisioning material")
			}
		case <-time.After(100 * time.Millisecond):
			_ = hostConnection.Close()
			<-result
			t.Fatal("guest attempted an ACK instead of immediately rejecting all-zero material")
		}
		clearProvision(provision)
		_ = hostConnection.Close()
	})

	t.Run("all-zero guest nonce", func(t *testing.T) {
		hostConnection, guestConnection := net.Pipe()
		result := make(chan error, 1)
		go func() {
			_, err := acceptGuest(
				context.Background(),
				guestConnection,
				testMaximumControl,
				time.Second,
				bytes.NewReader(make([]byte, NonceSize)),
			)
			result <- err
		}()
		provision := &ProvisionChannel{
			ProtocolVersion: ProtocolVersion,
			BootId:          bytes.Repeat([]byte{1}, BootIDSize),
			ChannelKey:      bytes.Repeat([]byte{2}, ChannelKeySize),
			HostNonce:       bytes.Repeat([]byte{3}, NonceSize),
		}
		encoded, err := writeMessage(hostConnection, MaxHandshakeBytes, provision)
		zero(encoded)
		if err != nil {
			t.Fatalf("write provision: %v", err)
		}
		select {
		case err := <-result:
			if err == nil {
				t.Fatal("guest accepted an all-zero CSPRNG nonce")
			}
		case <-time.After(100 * time.Millisecond):
			_ = hostConnection.Close()
			<-result
			t.Fatal("guest attempted an ACK with an all-zero nonce")
		}
		clearProvision(provision)
		_ = hostConnection.Close()
	})
}

func TestBootMaterialUsesFreshNonzeroCSPRNGValues(t *testing.T) {
	if material, err := newBootMaterial(bytes.NewReader(make([]byte, BootIDSize+ChannelKeySize+NonceSize))); err == nil || material != nil {
		t.Fatal("host accepted deterministic all-zero boot material")
	}
	first, err := NewBootMaterial()
	if err != nil {
		t.Fatal(err)
	}
	second, err := NewBootMaterial()
	if err != nil {
		t.Fatal(err)
	}
	if allZero(first.bootID[:]) || allZero(first.key[:]) || allZero(first.hostNonce[:]) {
		t.Fatal("boot material contains an all-zero value")
	}
	if bytes.Equal(first.bootID[:], second.bootID[:]) || bytes.Equal(first.key[:], second.key[:]) || bytes.Equal(first.hostNonce[:], second.hostNonce[:]) {
		t.Fatal("independent boot material reused a CSPRNG value")
	}
	first.Close()
	second.Close()
}

func newChannelPair(t *testing.T, maximumControl int) (*HostChannel, *GuestChannel) {
	t.Helper()
	hostConnection, guestConnection := net.Pipe()
	material, err := NewBootMaterial()
	if err != nil {
		t.Fatal(err)
	}
	guestResult := make(chan struct {
		channel *GuestChannel
		err     error
	}, 1)
	go func() {
		channel, err := AcceptGuest(context.Background(), guestConnection, maximumControl, time.Second)
		guestResult <- struct {
			channel *GuestChannel
			err     error
		}{channel: channel, err: err}
	}()
	host, err := ProvisionHost(context.Background(), hostConnection, material, maximumControl, time.Second)
	if err != nil {
		t.Fatalf("ProvisionHost: %v", err)
	}
	guest := <-guestResult
	if guest.err != nil {
		t.Fatalf("AcceptGuest: %v", guest.err)
	}
	t.Cleanup(func() {
		_ = host.Close()
		_ = guest.channel.Close()
	})
	return host, guest.channel
}

func validHostPayload(host *HostChannel, sequence uint64, requestID string, control []byte) *AuthenticatedPayload {
	return &AuthenticatedPayload{
		ProtocolVersion: ProtocolVersion,
		BootId:          append([]byte(nil), host.bootID[:]...),
		Direction:       ChannelDirection_CHANNEL_DIRECTION_HOST_TO_GUEST,
		Sequence:        sequence,
		RequestId:       requestID,
		DeadlineUnixMs:  uint64(time.Now().Add(time.Minute).UnixMilli()),
		ControlFrame:    append([]byte(nil), control...),
	}
}

func authenticatedFrame(t *testing.T, key *[ChannelKeySize]byte, payload *AuthenticatedPayload) *AuthenticatedFrame {
	t.Helper()
	authentication, err := payloadMAC(key, payload)
	if err != nil {
		t.Fatal(err)
	}
	return &AuthenticatedFrame{Payload: payload, HmacSha256: authentication}
}

func sendFrameAndReceive(t *testing.T, host *HostChannel, guest *GuestChannel, frame *AuthenticatedFrame) []byte {
	t.Helper()
	writeResult := make(chan struct {
		canonical []byte
		err       error
	}, 1)
	go func() {
		canonical, err := writeMessage(host.connection, host.maximumFrame, frame)
		writeResult <- struct {
			canonical []byte
			err       error
		}{canonical: canonical, err: err}
	}()
	request, err := guest.Receive(context.Background())
	if err != nil {
		t.Fatalf("Receive: %v", err)
	}
	written := <-writeResult
	if written.err != nil {
		t.Fatalf("write request: %v", written.err)
	}
	if request == nil {
		t.Fatal("guest request is nil")
	}
	return written.canonical
}

func clearProvision(provision *ProvisionChannel) {
	if provision == nil {
		return
	}
	zero(provision.BootId)
	zero(provision.ChannelKey)
	zero(provision.HostNonce)
}

func allZero(data []byte) bool {
	var combined byte
	for _, value := range data {
		combined |= value
	}
	return combined == 0
}
