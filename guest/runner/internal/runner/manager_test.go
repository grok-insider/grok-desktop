package runner

import (
	"encoding/json"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/grok-insider/grok-desktop/native/windows-vm-service/manifestverify"
)

func TestEnsureIntegrationStateRejectsPathIdentities(t *testing.T) {
	root := t.TempDir()
	for _, id := range []string{".", "..", "single", "Desktop.Grok.Wisp", "desktop/grok", "desktop\\grok"} {
		if _, err := ensureIntegrationState(root, id); err == nil {
			t.Fatalf("unsafe integration identity %q was accepted", id)
		}
	}
	path, err := ensureIntegrationState(root, "desktop.grok-wisp")
	if err != nil {
		t.Fatal(err)
	}
	if path != filepath.Join(root, "desktop.grok-wisp") {
		t.Fatalf("unexpected state path: %s", path)
	}
}

func TestSandboxArgumentsBindOnlyGrantedWorkspaceDescriptors(t *testing.T) {
	bundle := &VerifiedBundle{Manifest: &manifestverify.Manifest{
		Entrypoint: manifestverify.Entrypoint{Command: "bin/adapter", Arguments: []string{"--stdio"}},
	}}
	policy := Policy{WorkspaceRoot: "/run/grok-desktop/workspaces"}
	workspaces := []Workspace{{
		MountID: "project", Path: "/run/grok-desktop/workspaces/project", ReadOnly: true,
	}}
	arguments, err := sandboxArguments(policy, bundle, "/var/lib/grok-integrations/desktop.grok-wisp", workspaces)
	if err != nil {
		t.Fatal(err)
	}
	joined, _ := json.Marshal(arguments)
	for index, argument := range arguments {
		if (argument == "--ro-bind" || argument == "--ro-bind-fd" || argument == "--bind" || argument == "--bind-fd") &&
			index+2 < len(arguments) && arguments[index+2] == policy.WorkspaceRoot {
			t.Fatalf("entire workspace root was exposed: %s", joined)
		}
	}
	want := []string{"--ro-bind-fd", "5", workspaces[0].Path}
	found := false
	for index := 0; index+len(want) <= len(arguments); index++ {
		if slices.Equal(arguments[index:index+len(want)], want) {
			found = true
			break
		}
	}
	if !found || !slices.Contains(arguments, "--disable-userns") || !slices.Contains(arguments, "--seccomp") {
		t.Fatalf("sandbox descriptor or namespace boundary is absent: %s", joined)
	}
	seccomp := []string{"--seccomp", "6"}
	found = false
	for index := 0; index+len(seccomp) <= len(arguments); index++ {
		if slices.Equal(arguments[index:index+len(seccomp)], seccomp) {
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("seccomp descriptor does not follow pinned resources: %s", joined)
	}
}

func TestAuthorizeComputerUseReadsFullActionEnvelope(t *testing.T) {
	params := json.RawMessage(`{
		"protocol":"grok.computer-use/v1",
		"type":"action",
		"actionId":"action-1",
		"observationRevision":1,
		"application":{"applicationId":"app","instanceId":"instance"},
		"action":{"kind":"pointer.click","at":{"x":1,"y":2},"button":"primary","count":1}
	}`)
	grants := map[string]struct{}{"computer-use.pointer": {}}
	if err := authorizeComputerUse(grants, "computer-use.act", params); err != nil {
		t.Fatalf("valid full action envelope was rejected: %v", err)
	}
	if err := authorizeComputerUse(map[string]struct{}{}, "computer-use.act", params); err == nil {
		t.Fatal("action without a grant was accepted")
	}
}

func TestValidateComputerUseResponseRequiresMethodSpecificShape(t *testing.T) {
	if err := validateComputerUseResponse("computer-use.observe", json.RawMessage(`{}`), json.RawMessage(`{"protocol":"grok.computer-use/v1","type":"observation"}`)); err != nil {
		t.Fatalf("observation response was rejected: %v", err)
	}
	if err := validateComputerUseResponse("computer-use.observe", json.RawMessage(`{}`), json.RawMessage(`{"protocol":"grok.computer-use/v1","type":"action-result"}`)); err == nil {
		t.Fatal("action result was accepted for an observation request")
	}
}

func TestValidateComputerUseResponseCorrelatesExactActionTarget(t *testing.T) {
	request := json.RawMessage(`{
		"protocol":"grok.computer-use/v1","type":"action","actionId":"action-1","observationRevision":7,
		"application":{"applicationId":"app","instanceId":"instance","processId":42,"windowId":"window","title":"before"}
	}`)
	valid := json.RawMessage(`{
		"protocol":"grok.computer-use/v1","type":"action-result","actionId":"action-1","observationRevision":7,
		"application":{"applicationId":"app","instanceId":"instance","processId":42,"windowId":"window","title":"after"}
	}`)
	if err := validateComputerUseResponse("computer-use.act", request, valid); err != nil {
		t.Fatalf("correlated action result was rejected: %v", err)
	}
	for name, response := range map[string]json.RawMessage{
		"type":        json.RawMessage(strings.Replace(string(valid), `"action-result"`, `"observation"`, 1)),
		"action":      json.RawMessage(strings.Replace(string(valid), `"action-1"`, `"action-2"`, 1)),
		"revision":    json.RawMessage(strings.Replace(string(valid), `"observationRevision":7`, `"observationRevision":8`, 1)),
		"application": json.RawMessage(strings.Replace(string(valid), `"applicationId":"app"`, `"applicationId":"other"`, 1)),
		"instance":    json.RawMessage(strings.Replace(string(valid), `"instanceId":"instance"`, `"instanceId":"other"`, 1)),
		"process":     json.RawMessage(strings.Replace(string(valid), `"processId":42`, `"processId":43`, 1)),
		"window":      json.RawMessage(strings.Replace(string(valid), `"windowId":"window"`, `"windowId":"other"`, 1)),
	} {
		t.Run(name, func(t *testing.T) {
			if err := validateComputerUseResponse("computer-use.act", request, response); err == nil {
				t.Fatal("mismatched action result was accepted")
			}
		})
	}
}
