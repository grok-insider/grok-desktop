# ADR 0003: Fail-closed managed execution on Windows

- Status: accepted
- Date: 2026-07-10

## Context

Approvals do not contain malicious code or prompt-injected tools. Native Windows
ACP sandboxing is not a documented Grok Build security boundary, and direct
writable host mounts make path and race validation unreliable.

## Decision

Run strong Work tools in a managed Linux utility VM using HCS and Windows
VirtualMachinePlatform. Share validated host content read-only, collect writes
in a guest overlay, and commit reviewed diffs through the daemon. Broker network
access through the host without a general guest NIC.

When the backend cannot be qualified or started, expose Limited Mode and disable
local tools, filesystem authority, MCP, browser control, and computer use. Do
not fall back to unrestricted host execution.

## Consequences

Strong Work can require elevation, virtualization support, disk space, and an
initial image download. The application must make readiness and degradation
visible, support enterprise proxy/custom-CA environments, and test recovery of
the service, guest, and image independently.

