package manifestverify

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
)

func TestVerifyAllowsBoundedDeclaredPermissions(t *testing.T) {
	manifest, privateKey, policy := validManifest(t)
	manifest.Publisher.URL = "https://example.com/integrations/publisher"
	manifest.Permissions = Permissions{
		Filesystem: FilesystemPermissions{
			ReadOnlyRoots: []string{"/run/workspaces"}, ReadWriteRoots: []string{"/var/lib/integration"},
		},
		Network: NetworkPermissions{
			Outbound: []NetworkEndpoint{{Host: "api.example.com", Ports: []int{80}, TLS: false}},
			Listen:   []ListenEndpoint{{Family: "unix", Address: "/run/integration/control.sock"}},
		},
		Process:          ProcessPermissions{Spawn: []string{"adapter-helper"}},
		Devices:          []string{"virtual-audio"},
		Secrets:          []string{"api.key"},
		HostCapabilities: []string{"guest-socket:control"},
	}
	if _, err := Verify(signManifest(t, manifest, privateKey), policy); err != nil {
		t.Fatalf("Verify bounded permissions: %v", err)
	}
}

func TestProtocolRangeUsesSemanticPrereleaseOrdering(t *testing.T) {
	manifest, privateKey, policy := validManifest(t)
	manifest.Protocol = ProtocolRange{MinInclusive: "1.0.0-rc.2", MaxExclusive: "1.0.0"}
	policy.SupportedProtocol = "1.0.0-rc.10"
	if _, err := Verify(signManifest(t, manifest, privateKey), policy); err != nil {
		t.Fatalf("Verify prerelease protocol range: %v", err)
	}
	policy.SupportedProtocol = "1.0.0"
	assertVerificationCode(t, verifyResult(signManifest(t, manifest, privateKey), policy), CodeIncompatibleProtocol)
}

func TestVerifyRejectsInvalidSignatureMetadata(t *testing.T) {
	for _, test := range []struct {
		name   string
		mutate func(*Manifest)
	}{
		{"value encoding", func(m *Manifest) {
			value := "not-base64"
			m.Signature.Value = &value
		}},
		{"unsigned key metadata", func(m *Manifest) {
			keyID, value := "key", "value"
			m.UpdateChannel = "development"
			m.Signature = Signature{Algorithm: "none", KeyID: &keyID, Value: &value}
		}},
	} {
		t.Run(test.name, func(t *testing.T) {
			manifest, _, policy := validManifest(t)
			test.mutate(&manifest)
			assertVerificationCode(t, verifyResult(marshalManifest(t, manifest), policy), CodeInvalidSignature)
		})
	}
}

func TestVerifyRejectsSchemaViolations(t *testing.T) {
	tests := []struct {
		name   string
		code   ErrorCode
		mutate func(*Manifest)
	}{
		{"manifest id syntax", CodeInvalidManifest, func(m *Manifest) { m.ID = "single" }},
		{"manifest id length", CodeInvalidManifest, func(m *Manifest) { m.ID = "a." + strings.Repeat("b", 127) }},
		{"version syntax", CodeInvalidManifest, func(m *Manifest) { m.Version = "1.0" }},
		{"version build identifier", CodeInvalidManifest, func(m *Manifest) { m.Version = "1.0.0+build..id" }},
		{"version prerelease leading zero", CodeInvalidManifest, func(m *Manifest) { m.Version = "1.0.0-01" }},
		{"version length", CodeInvalidManifest, func(m *Manifest) { m.Version = "1.0.0+" + strings.Repeat("a", 65) }},
		{"protocol range", CodeInvalidManifest, func(m *Manifest) {
			m.Protocol = ProtocolRange{MinInclusive: "2.0.0", MaxExclusive: "2.0.0"}
		}},
		{"entrypoint argument count", CodeInvalidManifest, func(m *Manifest) {
			m.Entrypoint.Arguments = make([]string, 17)
		}},
		{"entrypoint argument length", CodeInvalidManifest, func(m *Manifest) {
			m.Entrypoint.Arguments = []string{strings.Repeat("a", 257)}
		}},
		{"entrypoint argument control", CodeInvalidManifest, func(m *Manifest) {
			m.Entrypoint.Arguments = []string{"--value\nprivate"}
		}},
		{"entrypoint command charset", CodeUnsafePath, func(m *Manifest) { m.Entrypoint.Command = "bin/my adapter" }},
		{"entrypoint adapter extension", CodeUnsafePath, func(m *Manifest) { m.Entrypoint.Adapter = "adapter.yaml" }},
		{"config extension", CodeUnsafePath, func(m *Manifest) { m.ConfigSchema = "config.schema.JSON" }},
		{"publisher id", CodeInvalidManifest, func(m *Manifest) { m.Publisher.ID = "Grok" }},
		{"publisher name empty", CodeInvalidManifest, func(m *Manifest) { m.Publisher.Name = "" }},
		{"publisher name control", CodeInvalidManifest, func(m *Manifest) { m.Publisher.Name = "private\nname" }},
		{"publisher trust enum", CodeInvalidManifest, func(m *Manifest) { m.Publisher.Trust = "official" }},
		{"publisher trust policy", CodeUntrustedSignature, func(m *Manifest) { m.Publisher.Trust = "third-party" }},
		{"publisher URL", CodeInvalidManifest, func(m *Manifest) { m.Publisher.URL = "relative/path" }},
		{"signature algorithm", CodeInvalidSignature, func(m *Manifest) { m.Signature.Algorithm = "rsa" }},
		{"signature key id", CodeInvalidSignature, func(m *Manifest) {
			value := "invalid key"
			m.Signature.KeyID = &value
		}},
		{"update channel", CodeInvalidManifest, func(m *Manifest) { m.UpdateChannel = "release" }},
		{"capabilities missing", CodeInvalidManifest, func(m *Manifest) { m.Capabilities = nil }},
		{"capability count", CodeInvalidManifest, func(m *Manifest) {
			m.Capabilities = make([]string, 65)
			for index := range m.Capabilities {
				m.Capabilities[index] = "capability.item" + strings.Repeat("x", index%2)
			}
		}},
		{"capability syntax", CodeInvalidManifest, func(m *Manifest) { m.Capabilities = []string{"Host.Exec"} }},
		{"capability duplicate", CodeInvalidManifest, func(m *Manifest) {
			m.Capabilities = []string{"computer-use.observe", "computer-use.observe"}
		}},
		{"filesystem roots missing", CodeInvalidManifest, func(m *Manifest) { m.Permissions.Filesystem.ReadOnlyRoots = nil }},
		{"filesystem path", CodeUnsafePath, func(m *Manifest) {
			m.Permissions.Filesystem.ReadOnlyRoots = []string{"/workspace/../secret"}
		}},
		{"filesystem duplicate", CodeUnsafePath, func(m *Manifest) {
			m.Permissions.Filesystem.ReadOnlyRoots = []string{"/workspace", "/workspace"}
		}},
		{"filesystem count", CodeUnsafePath, func(m *Manifest) {
			m.Permissions.Filesystem.ReadOnlyRoots = guestPaths("/ro", 33)
		}},
		{"filesystem authority overlap", CodeUnsafePath, func(m *Manifest) {
			m.Permissions.Filesystem.ReadOnlyRoots = []string{"/workspace"}
			m.Permissions.Filesystem.ReadWriteRoots = []string{"/workspace/output"}
		}},
		{"outbound count", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.Network.Outbound = make([]NetworkEndpoint, 33)
		}},
		{"outbound hostname", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.Network.Outbound = []NetworkEndpoint{{Host: "Example.COM", Ports: []int{443}, TLS: true}}
		}},
		{"outbound ports empty", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.Network.Outbound = []NetworkEndpoint{{Host: "example.com", Ports: []int{}, TLS: true}}
		}},
		{"outbound port range", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.Network.Outbound = []NetworkEndpoint{{Host: "example.com", Ports: []int{65536}, TLS: true}}
		}},
		{"outbound duplicate port", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.Network.Outbound = []NetworkEndpoint{{Host: "example.com", Ports: []int{443, 443}, TLS: true}}
		}},
		{"outbound semantic duplicate", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.Network.Outbound = []NetworkEndpoint{
				{Host: "example.com", Ports: []int{80, 443}, TLS: true},
				{Host: "example.com", Ports: []int{443, 80}, TLS: true},
			}
		}},
		{"listen count", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.Network.Listen = make([]ListenEndpoint, 9)
		}},
		{"listen family", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.Network.Listen = []ListenEndpoint{{Family: "tcp", Address: "/run/socket"}}
		}},
		{"listen path", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.Network.Listen = []ListenEndpoint{{Family: "unix", Address: "run/socket"}}
		}},
		{"listen duplicate", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.Network.Listen = []ListenEndpoint{
				{Family: "unix", Address: "/run/socket"}, {Family: "unix", Address: "/run/socket"},
			}
		}},
		{"spawn missing", CodeInvalidManifest, func(m *Manifest) { m.Permissions.Process.Spawn = nil }},
		{"spawn count", CodeInvalidManifest, func(m *Manifest) { m.Permissions.Process.Spawn = make([]string, 33) }},
		{"spawn path", CodeInvalidManifest, func(m *Manifest) { m.Permissions.Process.Spawn = []string{"bin/tool"} }},
		{"spawn duplicate", CodeInvalidManifest, func(m *Manifest) { m.Permissions.Process.Spawn = []string{"tool", "tool"} }},
		{"device enum", CodeInvalidManifest, func(m *Manifest) { m.Permissions.Devices = []string{"host-gpu"} }},
		{"device duplicate", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.Devices = []string{"virtual-input", "virtual-input"}
		}},
		{"secret syntax", CodeInvalidManifest, func(m *Manifest) { m.Permissions.Secrets = []string{"A"} }},
		{"secret count", CodeInvalidManifest, func(m *Manifest) { m.Permissions.Secrets = make([]string, 17) }},
		{"secret duplicate", CodeInvalidManifest, func(m *Manifest) { m.Permissions.Secrets = []string{"api.key", "api.key"} }},
		{"host capability enum", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.HostCapabilities = []string{"host-process"}
		}},
		{"host capability duplicate", CodeInvalidManifest, func(m *Manifest) {
			m.Permissions.HostCapabilities = []string{"guest-socket:control", "guest-socket:control"}
		}},
		{"lifecycle scope", CodeInvalidManifest, func(m *Manifest) { m.Lifecycle.Scope = "desktop" }},
		{"restart policy", CodeInvalidManifest, func(m *Manifest) { m.Lifecycle.RestartPolicy = "sometimes" }},
		{"shutdown timeout", CodeInvalidManifest, func(m *Manifest) { m.Lifecycle.ShutdownTimeoutMS = 99 }},
		{"health method", CodeInvalidManifest, func(m *Manifest) { m.Lifecycle.HealthCheck.Method = "exec" }},
		{"health interval", CodeInvalidManifest, func(m *Manifest) { m.Lifecycle.HealthCheck.IntervalMS = 999 }},
		{"health timeout", CodeInvalidManifest, func(m *Manifest) { m.Lifecycle.HealthCheck.TimeoutMS = 30001 }},
		{"health threshold", CodeInvalidManifest, func(m *Manifest) { m.Lifecycle.HealthCheck.FailureThreshold = 11 }},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			manifest, privateKey, policy := validManifest(t)
			test.mutate(&manifest)
			encoded := signManifest(t, manifest, privateKey)
			assertVerificationCode(t, verifyResult(encoded, policy), test.code)
		})
	}
}

func TestDecodeRejectsMissingNullAndDuplicateStructure(t *testing.T) {
	tests := []struct {
		name   string
		mutate func(map[string]any)
	}{
		{"missing capabilities", func(value map[string]any) { delete(value, "capabilities") }},
		{"null capabilities", func(value map[string]any) { value["capabilities"] = nil }},
		{"missing arguments", func(value map[string]any) { delete(value["entrypoint"].(map[string]any), "arguments") }},
		{"null argument", func(value map[string]any) { value["entrypoint"].(map[string]any)["arguments"] = []any{nil} }},
		{"missing signature value", func(value map[string]any) { delete(value["signature"].(map[string]any), "value") }},
		{"null publisher URL", func(value map[string]any) { value["publisher"].(map[string]any)["url"] = nil }},
		{"empty publisher URL", func(value map[string]any) { value["publisher"].(map[string]any)["url"] = "" }},
		{"null schema reference", func(value map[string]any) { value["$schema"] = nil }},
		{"empty schema reference", func(value map[string]any) { value["$schema"] = "" }},
		{"missing devices", func(value map[string]any) { delete(value["permissions"].(map[string]any), "devices") }},
		{"null spawn", func(value map[string]any) {
			value["permissions"].(map[string]any)["process"].(map[string]any)["spawn"] = nil
		}},
		{"unknown nested permission", func(value map[string]any) {
			value["permissions"].(map[string]any)["network"].(map[string]any)["ambient"] = true
		}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			manifest, privateKey, policy := validManifest(t)
			var raw map[string]any
			if err := json.Unmarshal(signManifest(t, manifest, privateKey), &raw); err != nil {
				t.Fatal(err)
			}
			test.mutate(raw)
			encoded, err := json.Marshal(raw)
			if err != nil {
				t.Fatal(err)
			}
			assertVerificationCode(t, verifyResult(encoded, policy), CodeInvalidManifest)
		})
	}
}

func TestDecodeRejectsMissingOrNullTLS(t *testing.T) {
	for _, test := range []struct {
		name  string
		value any
	}{
		{"missing", "missing"},
		{"null", nil},
	} {
		t.Run(test.name, func(t *testing.T) {
			manifest, privateKey, policy := validManifest(t)
			manifest.Permissions.Network.Outbound = []NetworkEndpoint{{Host: "example.com", Ports: []int{443}, TLS: false}}
			var raw map[string]any
			if err := json.Unmarshal(signManifest(t, manifest, privateKey), &raw); err != nil {
				t.Fatal(err)
			}
			endpoint := raw["permissions"].(map[string]any)["network"].(map[string]any)["outbound"].([]any)[0].(map[string]any)
			if test.value == "missing" {
				delete(endpoint, "tls")
			} else {
				endpoint["tls"] = nil
			}
			encoded, err := json.Marshal(raw)
			if err != nil {
				t.Fatal(err)
			}
			assertVerificationCode(t, verifyResult(encoded, policy), CodeInvalidManifest)
		})
	}
}

func TestDecodeRejectsDuplicateJSONKey(t *testing.T) {
	manifest, privateKey, policy := validManifest(t)
	encoded := signManifest(t, manifest, privateKey)
	needle := []byte(`"manifestVersion":1`)
	duplicate := []byte(`"manifestVersion":1,"manifestVersion":1`)
	encoded = bytes.Replace(encoded, needle, duplicate, 1)
	assertVerificationCode(t, verifyResult(encoded, policy), CodeInvalidManifest)
}

func TestVerifyCheckedInWispDevelopmentManifest(t *testing.T) {
	_, currentFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("resolve test path")
	}
	repositoryRoot := filepath.Clean(filepath.Join(filepath.Dir(currentFile), "..", "..", ".."))
	data, err := os.ReadFile(filepath.Join(repositoryRoot, "integrations", "first-party", "wisp", "manifest.json"))
	if err != nil {
		t.Fatal(err)
	}
	policy := Policy{
		SupportedProtocol: "1.0.0",
		AllowedCapabilities: map[string]struct{}{
			"computer-use.observe": {}, "computer-use.pointer": {}, "computer-use.keyboard": {}, "computer-use.wait": {},
		},
		PublisherTrust:                map[string]string{"grok-insider": "first-party"},
		UnsignedDevelopmentPublishers: map[string]struct{}{"grok-insider": {}},
		AllowUnsignedDevelopment:      true,
	}
	manifest, err := Verify(data, policy)
	if err != nil {
		t.Fatalf("Verify checked-in Wisp manifest: %v", err)
	}
	if manifest.ID != "desktop.grok.wisp" || manifest.UpdateChannel != "development" {
		t.Fatalf("unexpected Wisp manifest identity")
	}
	delete(policy.UnsignedDevelopmentPublishers, "grok-insider")
	assertVerificationCode(t, verifyResult(data, policy), CodeUnsignedRelease)
}

func TestVerificationErrorsDoNotEchoManifestValues(t *testing.T) {
	manifest, privateKey, policy := validManifest(t)
	privateValue := "DO-NOT-ECHO-private-manifest-value"
	manifest.Publisher.Name = privateValue + "\n"
	err := verifyResult(signManifest(t, manifest, privateKey), policy)
	if err == nil {
		t.Fatal("expected verification error")
	}
	if strings.Contains(err.Error(), privateValue) {
		t.Fatalf("verification error exposed manifest content: %v", err)
	}
}

func FuzzDecodeManifest(f *testing.F) {
	f.Add([]byte(`{}`))
	f.Add([]byte(`null`))
	f.Add([]byte(`{"manifestVersion":1,"manifestVersion":1}`))
	f.Fuzz(func(t *testing.T, data []byte) {
		_, _ = Decode(data)
	})
}

func guestPaths(prefix string, count int) []string {
	values := make([]string, count)
	for index := range values {
		values[index] = prefix + "/" + strconv.Itoa(index)
	}
	return values
}
