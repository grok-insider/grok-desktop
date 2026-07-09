package guestchannelv2

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/binary"
	"errors"
	"io"
	"math"
	"net"
	"regexp"
	"time"

	"google.golang.org/protobuf/proto"
)

const (
	ProtocolVersion         uint32 = 2
	BootIDSize                     = 16
	ChannelKeySize                 = 32
	NonceSize                      = 32
	MACSize                        = sha256.Size
	MaxRequestIDBytes              = 128
	MaxHandshakeBytes              = 1024
	MaxReplayEntries               = 1024
	MaxReplayBytes                 = 64 << 20
	MaxRequestLifetime             = 2 * time.Minute
	DefaultHandshakeTimeout        = 10 * time.Second
)

const (
	frameMACDomain = "grok.desktop.guest-channel.v2"
	ackMACDomain   = "grok.desktop.guest-channel.v2.provision-ack"
)

var (
	errProtocol       = errors.New("guest channel protocol violation")
	errAuthentication = errors.New("guest channel authentication failed")
	requestIDPattern  = regexp.MustCompile(`^[A-Za-z0-9._:-]{1,128}$`)
	marshalOptions    = proto.MarshalOptions{Deterministic: true}
	unmarshalOptions  = proto.UnmarshalOptions{DiscardUnknown: false}
)

func readMessage(connection net.Conn, maximum int, destination proto.Message) ([]byte, error) {
	if maximum < 1 || maximum > math.MaxUint32 || destination == nil {
		return nil, errProtocol
	}
	var prefix [4]byte
	if _, err := io.ReadFull(connection, prefix[:]); err != nil {
		return nil, err
	}
	size := binary.BigEndian.Uint32(prefix[:])
	if size == 0 || uint64(size) > uint64(maximum) {
		return nil, errProtocol
	}
	wire := make([]byte, int(size))
	defer zero(wire)
	if _, err := io.ReadFull(connection, wire); err != nil {
		return nil, err
	}
	if err := unmarshalOptions.Unmarshal(wire, destination); err != nil {
		return nil, errProtocol
	}
	if len(destination.ProtoReflect().GetUnknown()) != 0 {
		return nil, errProtocol
	}
	canonical, err := marshalOptions.Marshal(destination)
	if err != nil || len(canonical) == 0 || len(canonical) > maximum {
		zero(canonical)
		return nil, errProtocol
	}
	return canonical, nil
}

func writeMessage(connection net.Conn, maximum int, message proto.Message) ([]byte, error) {
	if message == nil {
		return nil, errProtocol
	}
	canonical, err := marshalOptions.Marshal(message)
	if err != nil || len(canonical) == 0 || len(canonical) > maximum {
		zero(canonical)
		return nil, errProtocol
	}
	if err := writeCanonical(connection, maximum, canonical); err != nil {
		zero(canonical)
		return nil, err
	}
	return canonical, nil
}

func writeCanonical(connection net.Conn, maximum int, canonical []byte) error {
	if len(canonical) == 0 || len(canonical) > maximum || len(canonical) > math.MaxUint32 {
		return errProtocol
	}
	var prefix [4]byte
	binary.BigEndian.PutUint32(prefix[:], uint32(len(canonical)))
	if err := writeAll(connection, prefix[:]); err != nil {
		return err
	}
	return writeAll(connection, canonical)
}

func writeAll(writer io.Writer, data []byte) error {
	for len(data) > 0 {
		written, err := writer.Write(data)
		if err != nil {
			return err
		}
		if written < 1 || written > len(data) {
			return io.ErrShortWrite
		}
		data = data[written:]
	}
	return nil
}

func proofMAC(key *[ChannelKeySize]byte, proof *ProvisionChannelProof) ([]byte, error) {
	encoded, err := marshalOptions.Marshal(proof)
	if err != nil || len(encoded) == 0 || len(encoded) > MaxHandshakeBytes {
		zero(encoded)
		return nil, errProtocol
	}
	defer zero(encoded)
	return computeMAC(key, ackMACDomain, encoded), nil
}

func payloadMAC(key *[ChannelKeySize]byte, payload *AuthenticatedPayload) ([]byte, error) {
	encoded, err := marshalOptions.Marshal(payload)
	if err != nil || len(encoded) == 0 {
		zero(encoded)
		return nil, errProtocol
	}
	defer zero(encoded)
	return computeMAC(key, frameMACDomain, encoded), nil
}

func computeMAC(key *[ChannelKeySize]byte, domain string, encoded []byte) []byte {
	authenticator := hmac.New(sha256.New, key[:])
	_, _ = authenticator.Write([]byte(domain))
	_, _ = authenticator.Write([]byte{0})
	_, _ = authenticator.Write(encoded)
	return authenticator.Sum(nil)
}

func verifyPayloadMAC(key *[ChannelKeySize]byte, frame *AuthenticatedFrame) error {
	if frame == nil || frame.Payload == nil || len(frame.HmacSha256) != MACSize {
		return errAuthentication
	}
	expected, err := payloadMAC(key, frame.Payload)
	if err != nil {
		return err
	}
	defer zero(expected)
	if !hmac.Equal(expected, frame.HmacSha256) {
		return errAuthentication
	}
	return nil
}

func validatePayload(payload *AuthenticatedPayload, bootID *[BootIDSize]byte, direction ChannelDirection, maximumControl int, now time.Time) error {
	if payload == nil || payload.ProtocolVersion != ProtocolVersion || len(payload.BootId) != BootIDSize ||
		!hmac.Equal(payload.BootId, bootID[:]) || payload.Direction != direction || payload.Sequence == 0 ||
		!validRequestID(payload.RequestId) || len(payload.ControlFrame) == 0 || len(payload.ControlFrame) > maximumControl {
		return errProtocol
	}
	if payload.DeadlineUnixMs == 0 || payload.DeadlineUnixMs > math.MaxInt64 {
		return errProtocol
	}
	deadline := time.UnixMilli(int64(payload.DeadlineUnixMs))
	if !deadline.After(now) || deadline.After(now.Add(MaxRequestLifetime)) {
		return errProtocol
	}
	return nil
}

func validRequestID(value string) bool {
	return len(value) <= MaxRequestIDBytes && requestIDPattern.MatchString(value)
}

func frameMaximum(maximumControl int) (int, error) {
	if maximumControl < 1 || maximumControl > 16<<20 {
		return 0, errProtocol
	}
	const overhead = 1024
	if maximumControl > math.MaxInt-overhead {
		return 0, errProtocol
	}
	return maximumControl + overhead, nil
}

func setOperationDeadline(connection net.Conn, ctxDeadline time.Time, fallback time.Time) error {
	deadline := fallback
	if !ctxDeadline.IsZero() && (deadline.IsZero() || ctxDeadline.Before(deadline)) {
		deadline = ctxDeadline
	}
	return connection.SetDeadline(deadline)
}

func contextDeadline(ctx interface{ Deadline() (time.Time, bool) }) time.Time {
	deadline, present := ctx.Deadline()
	if !present {
		return time.Time{}
	}
	return deadline
}

func zero(data []byte) {
	for index := range data {
		data[index] = 0
	}
}
