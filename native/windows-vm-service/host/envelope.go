package host

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"regexp"
	"time"

	vmservice "github.com/grok-insider/grok-desktop/native/windows-vm-service"
)

const EnvelopeVersion = "1.0.0"

var (
	requestIDPattern      = regexp.MustCompile(`^[A-Za-z0-9._:-]{1,128}$`)
	idempotencyKeyPattern = regexp.MustCompile(`^[A-Za-z0-9._:-]{16,128}$`)
)

type RequestEnvelope struct {
	Version        string              `json:"version"`
	ID             string              `json:"id"`
	Operation      vmservice.Operation `json:"operation"`
	Deadline       string              `json:"deadline"`
	IdempotencyKey string              `json:"idempotencyKey,omitempty"`
	Payload        json.RawMessage     `json:"payload"`
}

type ResponseEnvelope struct {
	Version string          `json:"version"`
	ID      string          `json:"id,omitempty"`
	OK      bool            `json:"ok"`
	Result  json.RawMessage `json:"result,omitempty"`
	Error   *ResponseError  `json:"error,omitempty"`
}

type ResponseError struct {
	Code      string `json:"code"`
	Message   string `json:"message"`
	Retryable bool   `json:"retryable"`
}

type protocolError struct {
	code      string
	message   string
	retryable bool
}

func (e *protocolError) Error() string {
	return e.message
}

const (
	errorMalformedRequest    = "malformed_request"
	errorUnsupportedVersion  = "unsupported_version"
	errorUnknownOperation    = "unknown_operation"
	errorDeadlineExceeded    = "deadline_exceeded"
	errorDeadlineTooFar      = "deadline_too_far"
	errorIdempotencyRequired = "idempotency_key_required"
	errorIdempotencyConflict = "idempotency_conflict"
	errorServerBusy          = "server_busy"
	errorMessageTooLarge     = "message_too_large"
	errorInternal            = "internal"
)

type validatedEnvelope struct {
	request  RequestEnvelope
	deadline time.Time
}

func decodeEnvelope(data []byte, now time.Time, maxDeadline time.Duration) (validatedEnvelope, *protocolError) {
	var request RequestEnvelope
	if err := decodeStrictJSON(data, &request); err != nil {
		return validatedEnvelope{}, newProtocolError(errorMalformedRequest, "request envelope is not valid JSON: %v", err)
	}
	if request.Version != EnvelopeVersion {
		return validatedEnvelope{}, newProtocolError(errorUnsupportedVersion, "version must be %q", EnvelopeVersion)
	}
	if !requestIDPattern.MatchString(request.ID) {
		return validatedEnvelope{}, newProtocolError(errorMalformedRequest, "id must contain 1 to 128 safe characters")
	}
	if !knownOperation(request.Operation) {
		return validatedEnvelope{}, newProtocolError(errorUnknownOperation, "operation %q is not supported", request.Operation)
	}
	if len(bytes.TrimSpace(request.Payload)) == 0 || bytes.TrimSpace(request.Payload)[0] != '{' {
		return validatedEnvelope{}, newProtocolError(errorMalformedRequest, "payload must be a JSON object")
	}

	deadline, err := time.Parse(time.RFC3339Nano, request.Deadline)
	if err != nil {
		return validatedEnvelope{}, newProtocolError(errorMalformedRequest, "deadline must be an RFC3339 timestamp")
	}
	now = now.UTC()
	if !deadline.After(now) {
		return validatedEnvelope{}, &protocolError{code: errorDeadlineExceeded, message: "request deadline has expired", retryable: true}
	}
	if deadline.After(now.Add(maxDeadline)) {
		return validatedEnvelope{}, newProtocolError(errorDeadlineTooFar, "deadline cannot be more than %s in the future", maxDeadline)
	}

	if request.IdempotencyKey != "" && !idempotencyKeyPattern.MatchString(request.IdempotencyKey) {
		return validatedEnvelope{}, newProtocolError(errorMalformedRequest, "idempotencyKey must contain 16 to 128 safe characters")
	}
	if request.Operation != vmservice.OperationGetCapabilities && request.IdempotencyKey == "" {
		return validatedEnvelope{}, newProtocolError(errorIdempotencyRequired, "idempotencyKey is required for mutating operations")
	}

	return validatedEnvelope{request: request, deadline: deadline}, nil
}

func knownOperation(operation vmservice.Operation) bool {
	switch operation {
	case vmservice.OperationGetCapabilities,
		vmservice.OperationEnsureImage,
		vmservice.OperationCreateVM,
		vmservice.OperationStartVM,
		vmservice.OperationStopVM,
		vmservice.OperationDeleteVM,
		vmservice.OperationAttachWorkspace,
		vmservice.OperationGuestControl,
		vmservice.OperationOpenSocket:
		return true
	default:
		return false
	}
}

func decodeStrictJSON(data []byte, destination any) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(destination); err != nil {
		return err
	}
	if err := decoder.Decode(&struct{}{}); !errors.Is(err, io.EOF) {
		if err == nil {
			return errors.New("multiple JSON values are not allowed")
		}
		return err
	}
	return nil
}

func newProtocolError(code, format string, args ...any) *protocolError {
	return &protocolError{code: code, message: fmt.Sprintf(format, args...)}
}

func protocolErrorResponse(id string, err *protocolError) ResponseEnvelope {
	return ResponseEnvelope{
		Version: EnvelopeVersion,
		ID:      id,
		OK:      false,
		Error: &ResponseError{
			Code: err.code, Message: err.message, Retryable: err.retryable,
		},
	}
}
