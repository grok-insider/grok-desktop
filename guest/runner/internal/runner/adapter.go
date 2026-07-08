package runner

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"strings"

	"github.com/grok-insider/grok-desktop/guest/runner/internal/strictjson"
	jsonschema "github.com/santhosh-tekuri/jsonschema/v5"
)

type AdapterDescriptor struct {
	Schema         string           `json:"$schema,omitempty"`
	AdapterVersion int              `json:"adapterVersion"`
	Transport      AdapterTransport `json:"transport"`
	Protocol       AdapterProtocol  `json:"protocol"`
	Methods        AdapterMethods   `json:"methods"`
	Limits         AdapterLimits    `json:"limits"`
	Stderr         string           `json:"stderr"`
}

type AdapterTransport struct {
	Kind     string `json:"kind"`
	Encoding string `json:"encoding"`
	Framing  string `json:"framing"`
}

type AdapterProtocol struct {
	MinInclusive string `json:"minInclusive"`
	MaxExclusive string `json:"maxExclusive"`
}

type AdapterMethods struct {
	Lifecycle   []string `json:"lifecycle"`
	ComputerUse []string `json:"computerUse"`
}

type AdapterLimits struct {
	MaxMessageBytes     int `json:"maxMessageBytes"`
	MaxInflightRequests int `json:"maxInflightRequests"`
	InitializeTimeoutMS int `json:"initializeTimeoutMs"`
}

func parseAdapter(data []byte, policy Policy) (AdapterDescriptor, error) {
	var descriptor AdapterDescriptor
	if err := strictjson.Decode(data, 64<<10, &descriptor); err != nil {
		return AdapterDescriptor{}, fmt.Errorf("invalid adapter descriptor: %w", err)
	}
	if descriptor.AdapterVersion != 1 || descriptor.Transport.Kind != "stdio" ||
		descriptor.Transport.Encoding != "utf-8" || descriptor.Transport.Framing != "json-lines" ||
		descriptor.Stderr != "diagnostics-only" {
		return AdapterDescriptor{}, errors.New("unsupported adapter transport")
	}
	if descriptor.Protocol.MinInclusive != "1.0.0" || descriptor.Protocol.MaxExclusive != "2.0.0" {
		return AdapterDescriptor{}, errors.New("adapter protocol range is unsupported")
	}
	if !sameSet(descriptor.Methods.Lifecycle, []string{
		"lifecycle.initialize", "lifecycle.health", "lifecycle.shutdown",
	}) || !sameSet(descriptor.Methods.ComputerUse, []string{
		"computer-use.observe", "computer-use.act",
	}) {
		return AdapterDescriptor{}, errors.New("adapter method set is unsupported")
	}
	if descriptor.Limits.MaxMessageBytes < 4096 || descriptor.Limits.MaxMessageBytes > policy.MaxMessageBytes ||
		descriptor.Limits.MaxInflightRequests != 1 || descriptor.Limits.InitializeTimeoutMS < 100 ||
		descriptor.Limits.InitializeTimeoutMS > 30000 {
		return AdapterDescriptor{}, errors.New("adapter limits are invalid")
	}
	return descriptor, nil
}

func sameSet(actual, expected []string) bool {
	if len(actual) != len(expected) {
		return false
	}
	seen := make(map[string]struct{}, len(actual))
	for _, value := range actual {
		if _, duplicate := seen[value]; duplicate {
			return false
		}
		seen[value] = struct{}{}
	}
	for _, value := range expected {
		if _, exists := seen[value]; !exists {
			return false
		}
	}
	return true
}

func compileSchema(data []byte, resource string) (*jsonschema.Schema, error) {
	if err := strictjson.Validate(data, 256<<10); err != nil {
		return nil, fmt.Errorf("invalid JSON schema: %w", err)
	}
	var document any
	if err := strictjson.Decode(data, 256<<10, &document); err != nil {
		return nil, fmt.Errorf("invalid JSON schema: %w", err)
	}
	if err := rejectRemoteReferences(document); err != nil {
		return nil, err
	}
	compiler := jsonschema.NewCompiler()
	compiler.Draft = jsonschema.Draft2020
	compiler.LoadURL = func(string) (io.ReadCloser, error) {
		return nil, errors.New("external schema retrieval is disabled")
	}
	if err := compiler.AddResource(resource, bytes.NewReader(data)); err != nil {
		return nil, fmt.Errorf("load JSON schema: %w", err)
	}
	schema, err := compiler.Compile(resource)
	if err != nil {
		return nil, fmt.Errorf("compile JSON schema: %w", err)
	}
	return schema, nil
}

func rejectRemoteReferences(value any) error {
	switch typed := value.(type) {
	case map[string]any:
		for key, child := range typed {
			if key == "$ref" {
				reference, ok := child.(string)
				if !ok || !strings.HasPrefix(reference, "#") {
					return errors.New("external JSON schema references are disabled")
				}
			}
			if err := rejectRemoteReferences(child); err != nil {
				return err
			}
		}
	case []any:
		for _, child := range typed {
			if err := rejectRemoteReferences(child); err != nil {
				return err
			}
		}
	}
	return nil
}
