package host

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"sync"
	"sync/atomic"
	"time"

	vmservice "github.com/grok-insider/grok-desktop/native/windows-vm-service"
	"github.com/grok-insider/grok-desktop/native/windows-vm-service/transport"
)

const (
	DefaultMaxMessageBytes    = 8 << 20
	DefaultMaxRequestDeadline = 30 * time.Second
	DefaultIdleTimeout        = 2 * time.Minute
	DefaultWriteTimeout       = 5 * time.Second
	DefaultShutdownTimeout    = 5 * time.Second
	DefaultMaxConnections     = 16
	DefaultIdempotencyEntries = 1024
	DefaultIdempotencyTTL     = 10 * time.Minute
)

type Config struct {
	Service               vmservice.Service
	Resolver              ServiceResolver
	Logger                *slog.Logger
	MaxMessageBytes       int
	MaxRequestDeadline    time.Duration
	IdleTimeout           time.Duration
	WriteTimeout          time.Duration
	ShutdownTimeout       time.Duration
	MaxConnections        int
	IdempotencyMaxEntries int
	IdempotencyTTL        time.Duration
	Now                   func() time.Time
}

type connectionState struct {
	connection transport.Conn
	cancel     context.CancelFunc
}

type Server struct {
	config       Config
	resolver     ServiceResolver
	cache        *idempotencyCache
	logger       *slog.Logger
	shuttingDown atomic.Bool
	activeMu     sync.Mutex
	active       map[transport.Conn]connectionState
}

func New(config Config) (*Server, error) {
	if config.Service != nil && config.Resolver != nil {
		return nil, fmt.Errorf("Service and Resolver are mutually exclusive")
	}
	if config.Resolver == nil {
		if config.Service == nil {
			return nil, fmt.Errorf("service resolver is required")
		}
		config.Resolver = staticServiceResolver{service: config.Service}
	}
	applyConfigDefaults(&config)
	if config.MaxMessageBytes < 4096 || config.MaxMessageBytes > 8<<20 {
		return nil, fmt.Errorf("MaxMessageBytes must be between 4096 and 8388608")
	}
	if config.MaxRequestDeadline <= 0 || config.MaxRequestDeadline > 2*time.Minute {
		return nil, fmt.Errorf("MaxRequestDeadline must be between 1ns and 2m")
	}
	if config.MaxConnections < 1 || config.MaxConnections > 256 {
		return nil, fmt.Errorf("MaxConnections must be between 1 and 256")
	}
	if config.IdempotencyMaxEntries < 1 || config.IdempotencyMaxEntries > 65536 {
		return nil, fmt.Errorf("IdempotencyMaxEntries must be between 1 and 65536")
	}
	if config.IdleTimeout <= 0 || config.WriteTimeout <= 0 || config.ShutdownTimeout <= 0 || config.IdempotencyTTL <= 0 {
		return nil, fmt.Errorf("timeouts and idempotency TTL must be positive")
	}
	return &Server{
		config:   config,
		resolver: config.Resolver,
		cache:    newIdempotencyCache(config.IdempotencyMaxEntries, config.IdempotencyTTL),
		logger:   config.Logger,
		active:   make(map[transport.Conn]connectionState),
	}, nil
}

func applyConfigDefaults(config *Config) {
	if config.Logger == nil {
		config.Logger = slog.New(slog.NewTextHandler(io.Discard, nil))
	}
	if config.MaxMessageBytes == 0 {
		config.MaxMessageBytes = DefaultMaxMessageBytes
	}
	if config.MaxRequestDeadline == 0 {
		config.MaxRequestDeadline = DefaultMaxRequestDeadline
	}
	if config.IdleTimeout == 0 {
		config.IdleTimeout = DefaultIdleTimeout
	}
	if config.WriteTimeout == 0 {
		config.WriteTimeout = DefaultWriteTimeout
	}
	if config.ShutdownTimeout == 0 {
		config.ShutdownTimeout = DefaultShutdownTimeout
	}
	if config.MaxConnections == 0 {
		config.MaxConnections = DefaultMaxConnections
	}
	if config.IdempotencyMaxEntries == 0 {
		config.IdempotencyMaxEntries = DefaultIdempotencyEntries
	}
	if config.IdempotencyTTL == 0 {
		config.IdempotencyTTL = DefaultIdempotencyTTL
	}
	if config.Now == nil {
		config.Now = func() time.Time { return time.Now().UTC() }
	}
}

func (s *Server) Handle(ctx context.Context, peer transport.PeerIdentity, frame []byte) ResponseEnvelope {
	now := s.config.Now().UTC()
	validated, validationErr := decodeEnvelope(frame, now, s.config.MaxRequestDeadline)
	if validationErr != nil {
		return protocolErrorResponse(bestEffortRequestID(frame), validationErr)
	}
	request := validated.request
	requestContext, cancel := context.WithDeadline(ctx, validated.deadline)
	defer cancel()
	service, err := s.resolver.Resolve(requestContext, peer)
	if err != nil {
		return s.errorResponse(request.ID, request.Operation, err)
	}
	call, callErr := decodeCall(service, peer, request)
	if callErr != nil {
		return protocolErrorResponse(request.ID, callErr)
	}
	invoke := func() ResponseEnvelope {
		return s.invoke(requestContext, request.ID, request.Operation, call)
	}
	if request.IdempotencyKey == "" {
		return invoke()
	}

	cacheKey := transport.PrincipalCacheKey(peer) + "\x00" + string(request.Operation) + "\x00" + request.IdempotencyKey
	response, err := s.cache.do(requestContext, cacheKey, operationDigest(request.Operation, call.canonicalPayload), now, invoke)
	if err != nil {
		switch {
		case errors.Is(err, errIdempotencyConflict):
			return protocolErrorResponse(request.ID, newProtocolError(errorIdempotencyConflict, "idempotency key was already used for another request"))
		case errors.Is(err, errIdempotencyCapacity):
			return protocolErrorResponse(request.ID, &protocolError{code: errorServerBusy, message: "idempotency capacity is exhausted", retryable: true})
		case errors.Is(err, context.DeadlineExceeded), errors.Is(err, context.Canceled):
			return protocolErrorResponse(request.ID, &protocolError{code: errorDeadlineExceeded, message: "request ended while waiting for its idempotent result", retryable: true})
		default:
			return protocolErrorResponse(request.ID, &protocolError{code: errorInternal, message: "idempotency processing failed"})
		}
	}
	response.ID = request.ID
	return response
}

func (s *Server) invoke(ctx context.Context, id string, operation vmservice.Operation, call decodedCall) (response ResponseEnvelope) {
	defer func() {
		if recovered := recover(); recovered != nil {
			s.logger.Error("VM service operation panicked", "operation", operation)
			response = protocolErrorResponse(id, &protocolError{code: errorInternal, message: "service operation failed"})
		}
	}()
	if err := ctx.Err(); err != nil {
		return protocolErrorResponse(id, &protocolError{code: errorDeadlineExceeded, message: "request deadline expired", retryable: true})
	}
	result, err := call.invoke(ctx)
	if contextErr := ctx.Err(); contextErr != nil {
		return protocolErrorResponse(id, &protocolError{code: errorDeadlineExceeded, message: "request deadline expired", retryable: true})
	}
	if err != nil {
		return s.errorResponse(id, operation, err)
	}
	encoded, err := json.Marshal(result)
	if err != nil {
		s.logger.Error("VM service result could not be encoded", "operation", operation)
		return protocolErrorResponse(id, &protocolError{code: errorInternal, message: "service result could not be encoded"})
	}
	return ResponseEnvelope{Version: EnvelopeVersion, ID: id, OK: true, Result: encoded}
}

func (s *Server) errorResponse(id string, operation vmservice.Operation, err error) ResponseEnvelope {
	var serviceErr *vmservice.Error
	if errors.As(err, &serviceErr) {
		return ResponseEnvelope{
			Version: EnvelopeVersion, ID: id, OK: false,
			Error: &ResponseError{
				Code: string(serviceErr.Code), Message: publicServiceMessage(serviceErr.Code, serviceErr.Message),
				Retryable: serviceErr.Code == vmservice.CodeUnavailable,
			},
		}
	}
	if errors.Is(err, context.DeadlineExceeded) || errors.Is(err, context.Canceled) {
		return protocolErrorResponse(id, &protocolError{code: errorDeadlineExceeded, message: "request deadline expired", retryable: true})
	}
	s.logger.Error("VM service operation failed", "operation", operation)
	return protocolErrorResponse(id, &protocolError{code: errorInternal, message: "service operation failed"})
}

func (s *Server) Serve(ctx context.Context, listener transport.Listener) error {
	if listener == nil {
		return fmt.Errorf("listener is required")
	}
	defer listener.Close()

	serveDone := make(chan struct{})
	defer close(serveDone)
	go func() {
		select {
		case <-ctx.Done():
			s.shuttingDown.Store(true)
			_ = listener.Close()
			s.interruptReads()
		case <-serveDone:
		}
	}()

	semaphore := make(chan struct{}, s.config.MaxConnections)
	var handlers sync.WaitGroup
	var acceptErr error
	for {
		connection, err := listener.Accept()
		if err != nil {
			if ctx.Err() == nil && !errors.Is(err, net.ErrClosed) {
				acceptErr = fmt.Errorf("accept authenticated connection: %w", err)
			}
			break
		}
		select {
		case semaphore <- struct{}{}:
			handlerContext, cancel := context.WithCancel(context.Background())
			s.trackConnection(connection, cancel)
			handlers.Add(1)
			go func() {
				defer handlers.Done()
				defer func() { <-semaphore }()
				defer s.untrackConnection(connection)
				defer connection.Close()
				defer cancel()
				s.handleConnection(handlerContext, connection)
			}()
		default:
			// Authentication follows the first bounded read on Windows, so a
			// connection rejected at capacity receives no unauthenticated data.
			_ = connection.Close()
		}
	}
	s.shuttingDown.Store(true)
	_ = listener.Close()
	s.interruptReads()

	waitDone := make(chan struct{})
	go func() {
		handlers.Wait()
		close(waitDone)
	}()
	select {
	case <-waitDone:
	case <-time.After(s.config.ShutdownTimeout):
		s.forceCloseConnections()
		return fmt.Errorf("graceful shutdown exceeded %s", s.config.ShutdownTimeout)
	}
	return acceptErr
}

func (s *Server) handleConnection(ctx context.Context, connection transport.Conn) {
	reader := bufio.NewReaderSize(connection, min(s.config.MaxMessageBytes, 64<<10))
	var boundIdentity *transport.PeerIdentity
	authenticate := func() (transport.PeerIdentity, bool) {
		peer, err := connection.AuthenticatePeer()
		if err != nil {
			s.logger.Warn("peer authentication failed")
			return transport.PeerIdentity{}, false
		}
		if boundIdentity == nil {
			copy := peer
			boundIdentity = &copy
			return peer, true
		}
		if !transport.SamePrincipal(*boundIdentity, peer) {
			s.logger.Warn("peer logon changed on an established connection")
			return transport.PeerIdentity{}, false
		}
		return peer, true
	}
	for {
		if s.shuttingDown.Load() {
			return
		}
		_ = connection.SetReadDeadline(s.config.Now().Add(s.config.IdleTimeout))
		frame, err := readFrame(reader, s.config.MaxMessageBytes)
		if err != nil {
			if errors.Is(err, io.EOF) || isTimeout(err) {
				return
			}
			if errors.Is(err, errFrameTooLarge) {
				if _, ok := authenticate(); !ok {
					return
				}
				_ = connection.SetWriteDeadline(s.config.Now().Add(s.config.WriteTimeout))
				if writeErr := s.writeResponse(connection, protocolErrorResponse("", newProtocolError(errorMessageTooLarge, "request exceeds %d bytes", s.config.MaxMessageBytes))); writeErr != nil {
					return
				}
				continue
			}
			return
		}

		peer, ok := authenticate()
		if !ok {
			return
		}
		response := s.Handle(ctx, peer, frame)
		_ = connection.SetWriteDeadline(s.config.Now().Add(s.config.WriteTimeout))
		if err := s.writeResponse(connection, response); err != nil {
			return
		}
	}
}

func (s *Server) writeResponse(writer io.Writer, response ResponseEnvelope) error {
	encoded, err := json.Marshal(response)
	if err != nil {
		return fmt.Errorf("encode response envelope: %w", err)
	}
	if len(encoded) > s.config.MaxMessageBytes {
		encoded, err = json.Marshal(protocolErrorResponse(response.ID, &protocolError{
			code: errorInternal, message: "service response exceeds the message limit",
		}))
		if err != nil {
			return fmt.Errorf("encode bounded error response: %w", err)
		}
	}
	encoded = append(encoded, '\n')
	for len(encoded) > 0 {
		written, err := writer.Write(encoded)
		if err != nil {
			return err
		}
		if written == 0 {
			return io.ErrShortWrite
		}
		encoded = encoded[written:]
	}
	return nil
}

func (s *Server) trackConnection(connection transport.Conn, cancel context.CancelFunc) {
	s.activeMu.Lock()
	s.active[connection] = connectionState{connection: connection, cancel: cancel}
	s.activeMu.Unlock()
}

func (s *Server) untrackConnection(connection transport.Conn) {
	s.activeMu.Lock()
	delete(s.active, connection)
	s.activeMu.Unlock()
}

func (s *Server) interruptReads() {
	s.activeMu.Lock()
	defer s.activeMu.Unlock()
	for _, state := range s.active {
		_ = state.connection.SetReadDeadline(s.config.Now())
	}
}

func (s *Server) forceCloseConnections() {
	s.activeMu.Lock()
	defer s.activeMu.Unlock()
	for _, state := range s.active {
		state.cancel()
		_ = state.connection.Close()
	}
}

func isTimeout(err error) bool {
	var networkErr net.Error
	return errors.As(err, &networkErr) && networkErr.Timeout()
}

func bestEffortRequestID(frame []byte) string {
	var partial struct {
		ID string `json:"id"`
	}
	if json.Unmarshal(frame, &partial) == nil && requestIDPattern.MatchString(partial.ID) {
		return partial.ID
	}
	return ""
}
