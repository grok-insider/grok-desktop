package manifestverify

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
)

func rejectDuplicateJSONKeys(data []byte) error {
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	if err := consumeJSONValue(decoder, 0); err != nil {
		return verificationError(CodeInvalidManifest, "manifest JSON is malformed or contains duplicate object keys")
	}
	if _, err := decoder.Token(); !errors.Is(err, io.EOF) {
		return verificationError(CodeInvalidManifest, "manifest must contain one JSON object")
	}
	return nil
}

func consumeJSONValue(decoder *json.Decoder, depth int) error {
	if depth > 64 {
		return errors.New("JSON nesting exceeds limit")
	}
	token, err := decoder.Token()
	if err != nil {
		return err
	}
	delimiter, isDelimiter := token.(json.Delim)
	if !isDelimiter {
		return nil
	}
	switch delimiter {
	case '{':
		seen := make(map[string]struct{})
		for decoder.More() {
			keyToken, err := decoder.Token()
			if err != nil {
				return err
			}
			key, ok := keyToken.(string)
			if !ok {
				return errors.New("object key is not a string")
			}
			if _, duplicate := seen[key]; duplicate {
				return errors.New("duplicate object key")
			}
			seen[key] = struct{}{}
			if err := consumeJSONValue(decoder, depth+1); err != nil {
				return err
			}
		}
		closing, err := decoder.Token()
		if err != nil || closing != json.Delim('}') {
			return errors.New("unterminated object")
		}
	case '[':
		for decoder.More() {
			if err := consumeJSONValue(decoder, depth+1); err != nil {
				return err
			}
		}
		closing, err := decoder.Token()
		if err != nil || closing != json.Delim(']') {
			return errors.New("unterminated array")
		}
	default:
		return errors.New("unexpected delimiter")
	}
	return nil
}

// validateRequiredPresence closes the gap between JSON Schema required fields
// and Go zero values. In particular, a missing bool and false are otherwise
// indistinguishable, while null and a missing slice both decode to nil.
func validateRequiredPresence(data []byte) error {
	var manifest map[string]json.RawMessage
	if err := json.Unmarshal(data, &manifest); err != nil || manifest == nil {
		return invalidStructure()
	}
	if err := requireFields(manifest,
		"manifestVersion", "id", "version", "protocol", "entrypoint", "publisher", "signature",
		"capabilities", "configSchema", "permissions", "updateChannel", "lifecycle",
	); err != nil {
		return err
	}
	if raw, present := manifest["$schema"]; present {
		if isNull(raw) {
			return invalidStructure()
		}
		var value string
		if err := json.Unmarshal(raw, &value); err != nil || value == "" {
			return invalidStructure()
		}
	}
	if _, err := requiredArray(manifest, "capabilities"); err != nil {
		return err
	}

	protocol, err := requiredObject(manifest, "protocol")
	if err != nil {
		return err
	}
	if err := requireFields(protocol, "minInclusive", "maxExclusive"); err != nil {
		return err
	}

	entrypoint, err := requiredObject(manifest, "entrypoint")
	if err != nil {
		return err
	}
	if err := requireFields(entrypoint, "command", "arguments", "adapter"); err != nil {
		return err
	}
	arguments, err := requiredArray(entrypoint, "arguments")
	if err != nil {
		return err
	}
	for _, argument := range arguments {
		if isNull(argument) {
			return invalidStructure()
		}
		var value string
		if err := json.Unmarshal(argument, &value); err != nil {
			return invalidStructure()
		}
	}

	publisher, err := requiredObject(manifest, "publisher")
	if err != nil {
		return err
	}
	if err := requireFields(publisher, "id", "name", "trust"); err != nil {
		return err
	}
	if raw, present := publisher["url"]; present {
		if isNull(raw) {
			return invalidStructure()
		}
		var value string
		if err := json.Unmarshal(raw, &value); err != nil || value == "" {
			return invalidStructure()
		}
	}

	signature, err := requiredObject(manifest, "signature")
	if err != nil {
		return err
	}
	if err := requireFields(signature, "algorithm", "keyId", "value"); err != nil {
		return err
	}

	permissions, err := requiredObject(manifest, "permissions")
	if err != nil {
		return err
	}
	if err := requireFields(permissions, "filesystem", "network", "process", "devices", "secrets", "hostCapabilities"); err != nil {
		return err
	}
	for _, field := range []string{"devices", "secrets", "hostCapabilities"} {
		if _, err := requiredArray(permissions, field); err != nil {
			return err
		}
	}

	filesystem, err := requiredObject(permissions, "filesystem")
	if err != nil {
		return err
	}
	if err := requireFields(filesystem, "readOnlyRoots", "readWriteRoots"); err != nil {
		return err
	}
	for _, field := range []string{"readOnlyRoots", "readWriteRoots"} {
		if _, err := requiredArray(filesystem, field); err != nil {
			return err
		}
	}

	network, err := requiredObject(permissions, "network")
	if err != nil {
		return err
	}
	if err := requireFields(network, "outbound", "listen"); err != nil {
		return err
	}
	outbound, err := requiredArray(network, "outbound")
	if err != nil {
		return err
	}
	for _, raw := range outbound {
		endpoint, err := rawObject(raw)
		if err != nil {
			return err
		}
		if err := requireFields(endpoint, "host", "ports", "tls"); err != nil {
			return err
		}
		if _, err := requiredArray(endpoint, "ports"); err != nil {
			return err
		}
		tls := bytes.TrimSpace(endpoint["tls"])
		if !bytes.Equal(tls, []byte("true")) && !bytes.Equal(tls, []byte("false")) {
			return invalidStructure()
		}
	}
	listen, err := requiredArray(network, "listen")
	if err != nil {
		return err
	}
	for _, raw := range listen {
		endpoint, err := rawObject(raw)
		if err != nil {
			return err
		}
		if err := requireFields(endpoint, "family", "address"); err != nil {
			return err
		}
	}

	process, err := requiredObject(permissions, "process")
	if err != nil {
		return err
	}
	if err := requireFields(process, "spawn"); err != nil {
		return err
	}
	if _, err := requiredArray(process, "spawn"); err != nil {
		return err
	}

	lifecycle, err := requiredObject(manifest, "lifecycle")
	if err != nil {
		return err
	}
	if err := requireFields(lifecycle, "scope", "restartPolicy", "shutdownTimeoutMs", "healthCheck"); err != nil {
		return err
	}
	health, err := requiredObject(lifecycle, "healthCheck")
	if err != nil {
		return err
	}
	return requireFields(health, "method", "intervalMs", "timeoutMs", "failureThreshold")
}

func requiredObject(parent map[string]json.RawMessage, field string) (map[string]json.RawMessage, error) {
	raw, present := parent[field]
	if !present {
		return nil, invalidStructure()
	}
	return rawObject(raw)
}

func rawObject(raw json.RawMessage) (map[string]json.RawMessage, error) {
	var object map[string]json.RawMessage
	if err := json.Unmarshal(raw, &object); err != nil || object == nil {
		return nil, invalidStructure()
	}
	return object, nil
}

func requiredArray(parent map[string]json.RawMessage, field string) ([]json.RawMessage, error) {
	raw, present := parent[field]
	if !present {
		return nil, invalidStructure()
	}
	var values []json.RawMessage
	if err := json.Unmarshal(raw, &values); err != nil || values == nil {
		return nil, invalidStructure()
	}
	return values, nil
}

func requireFields(object map[string]json.RawMessage, fields ...string) error {
	for _, field := range fields {
		if _, present := object[field]; !present {
			return invalidStructure()
		}
	}
	return nil
}

func isNull(raw json.RawMessage) bool {
	return bytes.Equal(bytes.TrimSpace(raw), []byte("null"))
}

func invalidStructure() error {
	return verificationError(CodeInvalidManifest, "manifest is missing required typed structure")
}
