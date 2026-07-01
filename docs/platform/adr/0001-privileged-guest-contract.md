# ADR 0001: Narrow privileged guest and integration contracts

- Status: accepted
- Date: 2026-07-10
- Extends: `docs/decisions/0003-managed-execution.md`

## Context

Grok Desktop needs strong local execution and optional computer use on Windows.
Those workloads consume untrusted model output, files, web content, and tool
metadata. A broad privileged helper, writable host mount, general guest network,
or in-process add-on API would turn an application compromise into ambient host
authority.

The platform also needs to evolve independently: the desktop, Windows backend,
guest image, integration runner, and Wisp adapter have different release and
failure cycles.

## Decision

Use HCS/VirtualMachinePlatform to run a reproducible NixOS utility guest. The
native Windows service is a narrow privileged broker with exactly eight
versioned operations: capabilities, image ensure, VM create/start/stop/delete,
read-only workspace attach, and purpose-specific socket open.

The local service transport authenticates the current Windows SID and does not
trust an identity serialized by the caller. Clients grant exactly
`SecurityIdentification`, which lets the service query the connection token but
does not let it act as the client; stronger impersonation levels are rejected.
The token is reverted before dispatch or resource access. All filesystem
arguments are relative to fixed, service-owned roots. The API cannot execute
commands or accept arbitrary HCS configuration. Failure to initialize a
qualified backend returns unavailable and places the application in Limited
Mode.

This identification-only rule replaces the unreleased pre-qualification
implementation that required `SecurityImpersonation`. No production release
used that behavior. The broker does not accept both modes because a dual-mode
client could expose the user's token to a process that wins the fixed pipe name.
After the transport is released, an incompatible authentication change requires
a new pipe name and an explicit compatibility decision.

The guest has no general-purpose NIC or administrative login. Host content is
mounted read-only, guest writes stay in isolated state/overlays, and any host
commit is a separate daemon-authorized operation outside the VM service.

Managed integrations are signed bundles with strict manifests. They run out of
process under an integration runner and have an independent start, health,
restart, update, and stop lifecycle. Wisp is the recommended first-party
computer-use add-on, not a desktop or guest runtime dependency.

Adapters communicate over bounded UTF-8 JSON Lines. Computer-use protocol v1
uses monotonically increasing observation revisions, stable application IDs,
runtime instance IDs, and a closed pointer/keyboard/text/scroll/wait action
union. It does not include shell or process execution.

## Consequences

- Desktop installation and first use may require elevation, virtualization
  support, image storage, and a separately recoverable image download.
- A service, guest, or integration failure can disable Strong Work without
  taking down chat or corrupting desktop state.
- Supporting a new privileged feature requires an explicit contract revision,
  threat-model update, compatibility tests, and Windows qualification. It
  cannot be smuggled through a generic command field.
- Named-pipe clients must set identification-only SQOS explicitly. Requesting
  impersonation or delegation would expose the user's token to a fixed-name
  pipe squatter before server identity can be established.
- Workflows that need to modify host files use a guest overlay and reviewed
  diff commit, which adds latency but preserves a meaningful approval boundary.
- Wisp can be installed, updated, restarted, disabled, or removed without
  changing the desktop release or the base guest image.
- The repository can test VM lifecycle and policy behavior on non-Windows using
  a simulator, but simulator success is not evidence of isolation.

## Alternatives considered

### Run tools directly in Windows sandboxes

Rejected as the default boundary. The available policies do not provide one
qualified, reproducible isolation contract across ACP tools, MCP servers,
browsers, and computer-use dependencies.

### Expose PowerShell or generic HCS calls from the service

Rejected. Input validation would become an open-ended privileged language, and
same-user callers could convert a narrow broker into arbitrary host execution.

### Give the guest a NAT network and writable workspace share

Rejected. This creates ambient exfiltration and host modification paths that do
not correspond to the user's exact approval.

### Compile Wisp into the desktop or guest base image

Rejected. It would couple release, recovery, and trust of an optional
high-authority feature to the core chat runtime and make independent disable or
rollback impossible.

### Use a generic plugin protocol

Rejected for privileged actions. Capability-specific versioned contracts keep
the authority and compatibility surface reviewable; renderer extensibility is
not an integration goal.
