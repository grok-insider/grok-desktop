package linuxvmservice_test

import (
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"testing"

	linuxvmservice "github.com/grok-insider/grok-desktop/native/linux-vm-service"
)

// TestGuestControlWireRoundTrip freezes Go encoding/json body as base64 strings
// (canonical with Rust linux_guest_transport).
func TestGuestControlWireRoundTrip(t *testing.T) {
	t.Parallel()
	body := []byte(`{"status":"ok","vm":"work-vm","source":"lab-hook"}`)
	resp := linuxvmservice.GuestControlResponse{
		Method: "runner.health",
		Body:   body,
	}
	encoded, err := json.Marshal(resp)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(encoded, &raw); err != nil {
		t.Fatalf("raw: %v", err)
	}
	var bodyField string
	if err := json.Unmarshal(raw["body"], &bodyField); err != nil {
		t.Fatalf("body must be a JSON string (base64), got %s: %v", raw["body"], err)
	}
	decoded, err := base64.StdEncoding.DecodeString(bodyField)
	if err != nil {
		t.Fatalf("base64: %v", err)
	}
	if string(decoded) != string(body) {
		t.Fatalf("round-trip body mismatch: %q", decoded)
	}

	// Fixture produced for cross-language tests must match this encoding.
	fixture := loadWireFixture(t, "guest_control_success.jsonl")
	var envelope struct {
		OK     bool `json:"ok"`
		Result *struct {
			Method string `json:"method"`
			Body   string `json:"body"`
		} `json:"result"`
	}
	if err := json.Unmarshal(fixture, &envelope); err != nil {
		t.Fatalf("fixture: %v", err)
	}
	if !envelope.OK || envelope.Result == nil {
		t.Fatal("fixture must be a success envelope")
	}
	fixtureBody, err := base64.StdEncoding.DecodeString(envelope.Result.Body)
	if err != nil {
		t.Fatalf("fixture body base64: %v", err)
	}
	if string(fixtureBody) != string(body) {
		t.Fatalf("fixture body drift: got %q want %q", fixtureBody, body)
	}
}

func TestGuestControlErrorFixtureIsTyped(t *testing.T) {
	t.Parallel()
	fixture := loadWireFixture(t, "guest_control_error.jsonl")
	var envelope struct {
		OK    bool `json:"ok"`
		Error *struct {
			Code    string `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	if err := json.Unmarshal(fixture, &envelope); err != nil {
		t.Fatalf("fixture: %v", err)
	}
	if envelope.OK || envelope.Error == nil || envelope.Error.Code != "not_found" {
		t.Fatalf("unexpected error fixture: %+v", envelope)
	}
}

func loadWireFixture(t *testing.T, name string) []byte {
	t.Helper()
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("caller")
	}
	path := filepath.Join(filepath.Dir(file), "testdata", "wire", name)
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	// jsonl: single line
	return data
}
