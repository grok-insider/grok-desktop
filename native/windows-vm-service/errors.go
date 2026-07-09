package vmservice

import "fmt"

// ErrorCode is stable across transports. Transport adapters should preserve it.
type ErrorCode string

const (
	CodeInvalidArgument  ErrorCode = "invalid_argument"
	CodePermissionDenied ErrorCode = "permission_denied"
	CodeNotFound         ErrorCode = "not_found"
	CodeConflict         ErrorCode = "conflict"
	CodeUnavailable      ErrorCode = "unavailable"
)

// Error is the service's transport-neutral error shape.
type Error struct {
	Code    ErrorCode `json:"code"`
	Message string    `json:"message"`
}

func (e *Error) Error() string {
	return fmt.Sprintf("vm service: %s: %s", e.Code, e.Message)
}

func serviceError(code ErrorCode, format string, args ...any) error {
	return &Error{Code: code, Message: fmt.Sprintf(format, args...)}
}
