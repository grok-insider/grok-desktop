package host

import (
	"regexp"
	"strings"

	vmservice "github.com/grok-insider/grok-desktop/native/windows-vm-service"
)

var (
	diagnosticSIDPattern    = regexp.MustCompile(`(?i)\bS-1-[0-9]+(?:-[0-9]+)+\b`)
	diagnosticSecretPattern = regexp.MustCompile(`(?i)(bearer\s+|(?:token|secret|api[_-]?key)\s*[:=]\s*)[^\s,;]+`)
	diagnosticWindowsPath   = regexp.MustCompile(`(?i)(?:[a-z]:\\|\\\\)[^\r\n]+`)
	diagnosticUnixPath      = regexp.MustCompile(`(^|\s)/[^\r\n]+`)
)

func redactDiagnostic(message string) string {
	redacted := diagnosticSIDPattern.ReplaceAllString(message, "[sid]")
	redacted = diagnosticSecretPattern.ReplaceAllString(redacted, "${1}[redacted]")
	redacted = diagnosticWindowsPath.ReplaceAllString(redacted, "[path]")
	redacted = diagnosticUnixPath.ReplaceAllString(redacted, "${1}[path]")
	return redacted
}

func publicServiceMessage(code vmservice.ErrorCode, message string) string {
	if code == vmservice.CodeUnavailable {
		return "VM service is temporarily unavailable"
	}
	redacted := strings.TrimSpace(redactDiagnostic(message))
	if redacted == "" {
		return "VM service request failed"
	}
	return redacted
}
