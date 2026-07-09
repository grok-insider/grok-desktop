package transport

import (
	"strings"
	"testing"
)

func TestServicePipePolicyIsFixedAndDeniesRemoteIdentities(t *testing.T) {
	if ServicePipeName != `\\.\pipe\GrokDesktop.VMService.v1` {
		t.Fatalf("service pipe changed unexpectedly: %q", ServicePipeName)
	}
	for _, required := range []string{"D;;GA;;;AN", "D;;GA;;;NU", "A;;GA;;;SY", "A;;GRGW;;;AU"} {
		if !strings.Contains(servicePipeSDDL, required) {
			t.Fatalf("service pipe DACL is missing %q", required)
		}
	}
}
