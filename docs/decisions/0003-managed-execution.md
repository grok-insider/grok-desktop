# ADR 0003: Fail-closed managed execution

- Status: accepted
- Date: 2026-07-10
- Updated: 2026-07-12 (Linux host backend series)

## Context

Approvals do not contain malicious code or prompt-injected tools. Native Windows
ACP sandboxing is not a documented Grok Build security boundary, and direct
writable host mounts make path and race validation unreliable.

## Decision

Run strong Work tools in a managed **Linux utility VM** shared as guest OS
across host platforms:

- **Windows host:** HCS and Windows VirtualMachinePlatform with the LocalSystem
  VM service (existing platform ADRs 0001–0003).
- **Linux host:** privileged QEMU/KVM broker with the same narrow operation set
  and fail-closed Limited Mode
  ([platform ADR 0004](../platform/adr/0004-linux-qemu-kvm-managed-execution.md)
  through [0007](../platform/adr/0007-linux-workspace-share-and-host-commit.md)).

Share validated host content read-only, collect writes in a guest overlay, and
commit reviewed diffs through the daemon. Broker network access through the host
without a general guest NIC.

When the backend cannot be qualified or started, expose Limited Mode and disable
local tools, filesystem authority, MCP, browser control, and computer use. Do
not fall back to unrestricted host execution on any OS.

## Consequences

Strong Work can require elevation, virtualization support, disk space, and an
initial image download. The application must make readiness and degradation
visible, support enterprise proxy/custom-CA environments, and test recovery of
the service, guest, and image independently. Linux full product GA additionally
requires packaging of the broker and virtio guest image per
[linux-ga.md](../quality/linux-ga.md).

