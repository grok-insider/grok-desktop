package vmservice

import (
	"fmt"
	"path"
	"path/filepath"
	"regexp"
	"strings"
	"unicode"
)

var (
	idPattern            = regexp.MustCompile(`^[a-z][a-z0-9.-]{0,62}$`)
	sidPattern           = regexp.MustCompile(`(?i)^s-1-[0-9]+(?:-[0-9]+)+$`)
	sha256Pattern        = regexp.MustCompile(`(?i)^[0-9a-f]{64}$`)
	windowsVolumePattern = regexp.MustCompile(`^[a-zA-Z]:`)
)

var windowsReservedNames = map[string]struct{}{
	"CON": {}, "PRN": {}, "AUX": {}, "NUL": {},
	"COM1": {}, "COM2": {}, "COM3": {}, "COM4": {}, "COM5": {},
	"COM6": {}, "COM7": {}, "COM8": {}, "COM9": {},
	"LPT1": {}, "LPT2": {}, "LPT3": {}, "LPT4": {}, "LPT5": {},
	"LPT6": {}, "LPT7": {}, "LPT8": {}, "LPT9": {},
}

func validateID(kind, value string) error {
	if !idPattern.MatchString(value) {
		return serviceError(CodeInvalidArgument, "%s must match %s", kind, idPattern.String())
	}
	return nil
}

func validateRequest(request RequestIdentity, currentUserSID string) error {
	if request.RequestID == "" || len(request.RequestID) > 128 {
		return serviceError(CodeInvalidArgument, "requestId must contain 1 to 128 characters")
	}
	for _, r := range request.RequestID {
		if unicode.IsControl(r) {
			return serviceError(CodeInvalidArgument, "requestId must not contain control characters")
		}
	}
	if !sidPattern.MatchString(request.UserSID) {
		return serviceError(CodeInvalidArgument, "userSid is not a valid Windows SID")
	}
	if !strings.EqualFold(request.UserSID, currentUserSID) {
		return serviceError(CodePermissionDenied, "request SID does not match the service owner SID")
	}
	return nil
}

// resolveRelativePath applies Windows-safe lexical validation even when tests run
// on another OS. Native code must additionally resolve reparse points while the
// file handle is open before consuming a path.
func resolveRelativePath(root, value string) (string, error) {
	if value == "" {
		return "", serviceError(CodeInvalidArgument, "relative path is required")
	}
	if len(value) > 1024 {
		return "", serviceError(CodeInvalidArgument, "relative path exceeds 1024 bytes")
	}

	portable := strings.ReplaceAll(value, `\`, "/")
	if strings.HasPrefix(portable, "/") || windowsVolumePattern.MatchString(portable) {
		return "", serviceError(CodeInvalidArgument, "path must be relative to its service-owned root")
	}

	segments := strings.Split(portable, "/")
	for _, segment := range segments {
		if segment == "" || segment == "." || segment == ".." {
			return "", serviceError(CodeInvalidArgument, "path must be canonical and cannot traverse its root")
		}
		if strings.ContainsAny(segment, ":\x00") {
			return "", serviceError(CodeInvalidArgument, "path contains a Windows device or stream separator")
		}
		if len([]rune(segment)) > 240 {
			return "", serviceError(CodeInvalidArgument, "path segment exceeds 240 characters")
		}
		if strings.TrimRight(segment, " .") != segment {
			return "", serviceError(CodeInvalidArgument, "path segments cannot end in a dot or space")
		}
		base := strings.ToUpper(strings.SplitN(segment, ".", 2)[0])
		if _, reserved := windowsReservedNames[base]; reserved {
			return "", serviceError(CodeInvalidArgument, "path contains reserved Windows name %q", segment)
		}
	}

	clean := path.Clean(portable)
	candidate := filepath.Join(root, filepath.FromSlash(clean))
	relative, err := filepath.Rel(root, candidate)
	if err != nil {
		return "", serviceError(CodeInvalidArgument, "cannot resolve relative path: %v", err)
	}
	if relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) {
		return "", serviceError(CodePermissionDenied, "path escapes its service-owned root")
	}
	return clean, nil
}

func normalizeRoot(name, root string) (string, error) {
	if root == "" || !filepath.IsAbs(root) {
		return "", serviceError(CodeInvalidArgument, "%s must be an absolute service-owned path", name)
	}
	clean := filepath.Clean(root)
	if clean == string(filepath.Separator) {
		return "", serviceError(CodeInvalidArgument, "%s cannot be the filesystem root", name)
	}
	return clean, nil
}

func rootsOverlap(left, right string) bool {
	return pathWithinRoot(left, right) || pathWithinRoot(right, left)
}

func pathWithinRoot(root, candidate string) bool {
	relative, err := filepath.Rel(root, candidate)
	if err != nil {
		return false
	}
	return relative == "." || (relative != ".." && !strings.HasPrefix(relative, ".."+string(filepath.Separator)))
}

func validateSHA256(value string) (string, error) {
	if !sha256Pattern.MatchString(value) {
		return "", serviceError(CodeInvalidArgument, "sha256 must contain exactly 64 hexadecimal characters")
	}
	return strings.ToLower(value), nil
}

func validateSocketPurpose(value SocketPurpose) error {
	switch value {
	case SocketPurposeControl, SocketPurposeComputerUseV1:
		return nil
	default:
		return serviceError(CodeInvalidArgument, "unsupported socket purpose %q", value)
	}
}

func validateGuestControlMethod(value GuestControlMethod) error {
	switch value {
	case GuestControlRunnerHealth,
		GuestControlCatalogApply,
		GuestControlIntegrationStart,
		GuestControlIntegrationStop,
		GuestControlIntegrationCall:
		return nil
	default:
		return serviceError(CodeInvalidArgument, "unsupported guest control method %q", value)
	}
}

func contextErrorMessage(err error) error {
	return serviceError(CodeUnavailable, "request context ended: %v", err)
}

func copyVm(vm *Vm) Vm {
	result := *vm
	result.Workspaces = append([]WorkspaceAttachment(nil), vm.Workspaces...)
	return result
}

func socketID(sequence uint64) string {
	return fmt.Sprintf("socket-%06d", sequence)
}
