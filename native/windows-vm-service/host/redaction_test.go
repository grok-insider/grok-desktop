package host

import (
	"strings"
	"testing"

	vmservice "github.com/grok-insider/grok-desktop/native/windows-vm-service"
)

func TestPublicServiceMessageRedactsSensitiveDiagnostics(t *testing.T) {
	message := `failed C:\Users\Alice\workspace; token=top-secret; owner S-1-5-21-1-2-3-4`
	redacted := publicServiceMessage(vmservice.CodeInvalidArgument, message)
	for _, forbidden := range []string{"Alice", "top-secret", "S-1-5-21"} {
		if strings.Contains(redacted, forbidden) {
			t.Fatalf("public message leaked %q: %q", forbidden, redacted)
		}
	}
	if unavailable := publicServiceMessage(vmservice.CodeUnavailable, message); unavailable != "VM service is temporarily unavailable" {
		t.Fatalf("unavailable message = %q", unavailable)
	}
}
