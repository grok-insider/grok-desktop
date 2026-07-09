package hcsapi

import "context"

type System struct {
	ID        string `json:"Id"`
	Owner     string `json:"Owner"`
	State     string `json:"State"`
	RuntimeID string `json:"RuntimeId"`
	Type      string `json:"SystemType"`
}

// Client is the complete HCS surface used by the privileged service. It is
// intentionally lifecycle-only: callers cannot submit arbitrary modifications
// or launch a process in the guest.
type Client interface {
	Probe(context.Context) error
	Enumerate(context.Context, string) ([]System, error)
	Create(context.Context, string, []byte) error
	Start(context.Context, string) error
	Shutdown(context.Context, string) error
	Terminate(context.Context, string) error
	GrantVMAccess(context.Context, string, string) error
	RevokeVMAccess(context.Context, string, string) error
}

// Error preserves the HRESULT and bounded HCS result document for stable
// service-level error mapping without exposing handles or a generic API.
type Error struct {
	Operation string
	Code      uint32
	Document  string
}

func (e *Error) Error() string {
	if e.Document == "" {
		return e.Operation + " failed"
	}
	return e.Operation + " failed: " + e.Document
}
