package manifestverify

import (
	"fmt"
	"net/url"
	"path"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

var (
	manifestIDPattern     = regexp.MustCompile(`^[a-z][a-z0-9]*(?:[.-][a-z0-9]+)+$`)
	publisherIDPattern    = regexp.MustCompile(`^[a-z][a-z0-9.-]+$`)
	capabilityPattern     = regexp.MustCompile(`^[a-z][a-z0-9-]*(?:\.[a-z0-9-]+)+$`)
	bundlePathPattern     = regexp.MustCompile(`^[A-Za-z0-9._/-]+$`)
	guestPathPattern      = regexp.MustCompile(`^/[A-Za-z0-9._/-]+$`)
	hostnamePattern       = regexp.MustCompile(`^(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)(?:\.(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?))*$`)
	spawnPattern          = regexp.MustCompile(`^[A-Za-z0-9._+-]{1,64}$`)
	secretPattern         = regexp.MustCompile(`^[a-z][a-z0-9.-]{1,95}$`)
	signatureKeyIDPattern = regexp.MustCompile(`^[A-Za-z0-9._:-]{1,128}$`)
	signatureValuePattern = regexp.MustCompile(`^[A-Za-z0-9+/]{86}==$`)
)

func validateManifest(manifest *Manifest, policy Policy) error {
	if manifest.ManifestVersion != 1 {
		return invalidManifest("manifest version is unsupported")
	}
	if len(manifest.ID) > 128 || !manifestIDPattern.MatchString(manifest.ID) {
		return invalidManifest("integration id is invalid")
	}
	if err := validateSemanticVersion(manifest.Version); err != nil {
		return err
	}
	if manifest.Schema != "" {
		if !printableString(manifest.Schema, true) {
			return invalidManifest("schema reference is invalid")
		}
		if _, err := url.Parse(manifest.Schema); err != nil {
			return invalidManifest("schema reference is invalid")
		}
	}

	switch manifest.UpdateChannel {
	case "stable", "preview", "nightly", "development":
	default:
		return invalidManifest("update channel is unsupported")
	}
	if err := validateEntrypoint(manifest.Entrypoint, manifest.ConfigSchema); err != nil {
		return err
	}
	if err := validatePublisher(manifest.Publisher, policy); err != nil {
		return err
	}
	if err := validateSignatureMetadata(manifest.Signature); err != nil {
		return err
	}
	if err := validateProtocol(manifest.Protocol, policy.SupportedProtocol); err != nil {
		return err
	}
	if err := validateCapabilities(manifest.Capabilities, policy.AllowedCapabilities); err != nil {
		return err
	}
	if err := validatePermissions(manifest.Permissions); err != nil {
		return err
	}
	return validateLifecycle(manifest.Lifecycle)
}

func validateSemanticVersion(value string) error {
	if utf8.RuneCountInString(value) > 64 {
		return invalidManifest("semantic version exceeds its length limit")
	}
	if _, err := parseSemanticVersion(value); err != nil {
		return invalidManifest("semantic version is invalid")
	}
	return nil
}

func validateEntrypoint(entrypoint Entrypoint, configSchema string) error {
	if entrypoint.Arguments == nil || len(entrypoint.Arguments) > 16 {
		return invalidManifest("entrypoint arguments are missing or exceed their count limit")
	}
	for _, argument := range entrypoint.Arguments {
		if utf8.RuneCountInString(argument) > 256 || !printableString(argument, true) {
			return invalidManifest("entrypoint argument is not bounded printable text")
		}
	}
	for _, field := range []struct {
		value       string
		requireJSON bool
	}{
		{entrypoint.Command, false},
		{entrypoint.Adapter, true},
		{configSchema, true},
	} {
		if err := ValidateBundlePath(field.value); err != nil {
			return verificationError(CodeUnsafePath, "bundle path is invalid")
		}
		if field.requireJSON && !strings.HasSuffix(field.value, ".json") {
			return verificationError(CodeUnsafePath, "bundle JSON path has an invalid extension")
		}
	}
	return nil
}

func validatePublisher(publisher Publisher, policy Policy) error {
	if len(publisher.ID) > 128 || !publisherIDPattern.MatchString(publisher.ID) {
		return invalidManifest("publisher id is invalid")
	}
	if utf8.RuneCountInString(publisher.Name) < 1 || utf8.RuneCountInString(publisher.Name) > 128 || !printableString(publisher.Name, false) {
		return invalidManifest("publisher name is invalid")
	}
	if publisher.Trust != "first-party" && publisher.Trust != "third-party" {
		return invalidManifest("publisher trust classification is invalid")
	}
	expectedTrust, allowed := policy.PublisherTrust[publisher.ID]
	if !allowed || expectedTrust != publisher.Trust {
		return verificationError(CodeUntrustedSignature, "publisher trust does not match policy")
	}
	if publisher.URL != "" {
		parsed, err := url.ParseRequestURI(publisher.URL)
		if err != nil || !parsed.IsAbs() || !printableString(publisher.URL, false) {
			return invalidManifest("publisher URL is invalid")
		}
	}
	return nil
}

func validateSignatureMetadata(signature Signature) error {
	switch signature.Algorithm {
	case "none":
		if signature.KeyID != nil || signature.Value != nil {
			return verificationError(CodeInvalidSignature, "unsigned signature metadata is invalid")
		}
	case "ed25519":
		if signature.KeyID == nil || signature.Value == nil || !signatureKeyIDPattern.MatchString(*signature.KeyID) || !signatureValuePattern.MatchString(*signature.Value) {
			return verificationError(CodeInvalidSignature, "Ed25519 signature metadata is invalid")
		}
	default:
		return verificationError(CodeInvalidSignature, "signature algorithm is unsupported")
	}
	return nil
}

func validateCapabilities(capabilities []string, allowed map[string]struct{}) error {
	if capabilities == nil || len(capabilities) > 64 {
		return invalidManifest("capabilities are missing or exceed their count limit")
	}
	seen := make(map[string]struct{}, len(capabilities))
	for _, capability := range capabilities {
		if len(capability) > 96 || !capabilityPattern.MatchString(capability) {
			return invalidManifest("capability syntax is invalid")
		}
		if _, duplicate := seen[capability]; duplicate {
			return invalidManifest("capabilities contain a duplicate")
		}
		seen[capability] = struct{}{}
		if _, permitted := allowed[capability]; !permitted {
			return verificationError(CodeCapabilityEscalation, "capability is not allowed by policy")
		}
	}
	return nil
}

func validatePermissions(permissions Permissions) error {
	if err := validateFilesystem(permissions.Filesystem); err != nil {
		return err
	}
	if err := validateNetwork(permissions.Network); err != nil {
		return err
	}
	if err := validateSpawn(permissions.Process.Spawn); err != nil {
		return err
	}
	if err := validateEnumList(permissions.Devices, 3, map[string]struct{}{
		"wayland-virtual-display": {}, "virtual-input": {}, "virtual-audio": {},
	}, "device permissions"); err != nil {
		return err
	}
	if permissions.Secrets == nil || len(permissions.Secrets) > 16 {
		return invalidManifest("secret permissions are missing or exceed their count limit")
	}
	if err := validateUniqueStrings(permissions.Secrets, func(value string) bool { return secretPattern.MatchString(value) }); err != nil {
		return invalidManifest("secret permission syntax or uniqueness is invalid")
	}
	return validateEnumList(permissions.HostCapabilities, 2, map[string]struct{}{
		"guest-socket:control": {}, "guest-socket:computer-use-v1": {},
	}, "host capabilities")
}

func validateFilesystem(filesystem FilesystemPermissions) error {
	if filesystem.ReadOnlyRoots == nil || filesystem.ReadWriteRoots == nil || len(filesystem.ReadOnlyRoots) > 32 || len(filesystem.ReadWriteRoots) > 16 {
		return verificationError(CodeUnsafePath, "filesystem roots are missing or exceed their count limit")
	}
	if err := validateUniqueStrings(filesystem.ReadOnlyRoots, validGuestPath); err != nil {
		return verificationError(CodeUnsafePath, "read-only guest roots are invalid or duplicated")
	}
	if err := validateUniqueStrings(filesystem.ReadWriteRoots, validGuestPath); err != nil {
		return verificationError(CodeUnsafePath, "read-write guest roots are invalid or duplicated")
	}
	for _, readOnly := range filesystem.ReadOnlyRoots {
		for _, readWrite := range filesystem.ReadWriteRoots {
			if guestPathsOverlap(readOnly, readWrite) {
				return verificationError(CodeUnsafePath, "read-only and read-write guest roots overlap")
			}
		}
	}
	return nil
}

func validateNetwork(network NetworkPermissions) error {
	if network.Outbound == nil || network.Listen == nil || len(network.Outbound) > 32 || len(network.Listen) > 8 {
		return invalidManifest("network permissions are missing or exceed their count limit")
	}
	seenOutbound := make(map[string]struct{}, len(network.Outbound))
	for _, endpoint := range network.Outbound {
		if len(endpoint.Host) > 253 || !hostnamePattern.MatchString(endpoint.Host) || len(endpoint.Ports) == 0 {
			return invalidManifest("outbound network endpoint is invalid")
		}
		ports := append([]int(nil), endpoint.Ports...)
		seenPorts := make(map[int]struct{}, len(ports))
		for _, port := range ports {
			if port < 1 || port > 65535 {
				return invalidManifest("outbound network port is invalid")
			}
			if _, duplicate := seenPorts[port]; duplicate {
				return invalidManifest("outbound network ports contain a duplicate")
			}
			seenPorts[port] = struct{}{}
		}
		sort.Ints(ports)
		parts := make([]string, 0, len(ports))
		for _, port := range ports {
			parts = append(parts, strconv.Itoa(port))
		}
		key := endpoint.Host + "\x00" + strconv.FormatBool(endpoint.TLS) + "\x00" + strings.Join(parts, ",")
		if _, duplicate := seenOutbound[key]; duplicate {
			return invalidManifest("outbound network endpoints contain a duplicate")
		}
		seenOutbound[key] = struct{}{}
	}
	seenListen := make(map[string]struct{}, len(network.Listen))
	for _, endpoint := range network.Listen {
		if endpoint.Family != "unix" || !validGuestPath(endpoint.Address) {
			return invalidManifest("listen endpoint is invalid")
		}
		key := endpoint.Family + "\x00" + endpoint.Address
		if _, duplicate := seenListen[key]; duplicate {
			return invalidManifest("listen endpoints contain a duplicate")
		}
		seenListen[key] = struct{}{}
	}
	return nil
}

func validateSpawn(spawn []string) error {
	if spawn == nil || len(spawn) > 32 {
		return invalidManifest("spawn allowlist is missing or exceeds its count limit")
	}
	if err := validateUniqueStrings(spawn, func(value string) bool { return spawnPattern.MatchString(value) }); err != nil {
		return invalidManifest("spawn allowlist syntax or uniqueness is invalid")
	}
	return nil
}

func validateLifecycle(lifecycle Lifecycle) error {
	if lifecycle.Scope != "integration" {
		return invalidManifest("lifecycle scope is invalid")
	}
	if lifecycle.RestartPolicy != "never" && lifecycle.RestartPolicy != "on-failure" && lifecycle.RestartPolicy != "always" {
		return invalidManifest("restart policy is invalid")
	}
	if lifecycle.ShutdownTimeoutMS < 100 || lifecycle.ShutdownTimeoutMS > 30000 {
		return invalidManifest("shutdown timeout is outside its bounds")
	}
	health := lifecycle.HealthCheck
	if health.Method != "lifecycle.health" || health.IntervalMS < 1000 || health.IntervalMS > 300000 ||
		health.TimeoutMS < 100 || health.TimeoutMS > 30000 || health.FailureThreshold < 1 || health.FailureThreshold > 10 {
		return invalidManifest("health check configuration is invalid")
	}
	return nil
}

func validateEnumList(values []string, maximum int, allowed map[string]struct{}, field string) error {
	if values == nil || len(values) > maximum {
		return invalidManifest(field + " are missing or exceed their count limit")
	}
	seen := make(map[string]struct{}, len(values))
	for _, value := range values {
		if _, ok := allowed[value]; !ok {
			return invalidManifest(field + " contain an unsupported value")
		}
		if _, duplicate := seen[value]; duplicate {
			return invalidManifest(field + " contain a duplicate")
		}
		seen[value] = struct{}{}
	}
	return nil
}

func validateUniqueStrings(values []string, valid func(string) bool) error {
	seen := make(map[string]struct{}, len(values))
	for _, value := range values {
		if !valid(value) {
			return fmt.Errorf("invalid value")
		}
		if _, duplicate := seen[value]; duplicate {
			return fmt.Errorf("duplicate value")
		}
		seen[value] = struct{}{}
	}
	return nil
}

func validGuestPath(value string) bool {
	return len(value) <= 4096 && guestPathPattern.MatchString(value) && path.Clean(value) == value && value != "/"
}

func guestPathsOverlap(left, right string) bool {
	return left == right || strings.HasPrefix(left, right+"/") || strings.HasPrefix(right, left+"/")
}

func printableString(value string, allowEmpty bool) bool {
	if value == "" {
		return allowEmpty
	}
	for _, character := range value {
		if !unicode.IsPrint(character) {
			return false
		}
	}
	return true
}

func invalidManifest(message string) error {
	return verificationError(CodeInvalidManifest, "%s", message)
}
