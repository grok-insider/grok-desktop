# Architecture overview

Stable system design for Grok Desktop. For Clean Architecture / SOLID / coding
rules see [principles.md](principles.md). For crate ownership see
[modules.md](modules.md). For IPC epochs and SQLCipher schemas see
[protocol-and-persistence.md](protocol-and-persistence.md).

## Goals

Grok Desktop provides a durable Grok-only workspace while keeping provider,
platform, and presentation concerns independently replaceable. It is designed
for deterministic recovery, explicit authority, inspectable side effects, and
long-lived protocol compatibility.

## Process model

```text
Sandboxed React renderer
        |
Narrow Electron preload/main
        |
Versioned local Protobuf transport
        |
Rust per-user daemon (system of record)
   |          |          |          |
Grok ACP   xAI API    MCP/add-ons  policy brokers
                                      |
                      Windows service + isolated guest
```

The daemon can outlive a renderer restart and replays persisted events from a
monotonic cursor. Electron main supervises the daemon but does not interpret
domain state. Privileged service requests are separately authenticated and
never transit the renderer.

### Windows execution path

On Windows, the LocalSystem broker owns HCS and every HVSock handle. VM start
includes a fresh authenticated guest-channel handshake; service restart must
rekey an adopted runtime or stop it. The narrow `guest_control` proxy stays
unavailable to production callers until packaged qualification, proof of
possession, and durable replay recovery exist. No caller receives a raw guest
endpoint.

The application `IsolationProbe` port and `grok-vm-service-client` expose only
a bounded `get_capabilities` probe. A successful probe is a static broker
compatibility fact. It does not prove guest health, execution credential,
approval, or side-effect recovery, and therefore cannot enable Work or
`guest_control`.

### Credentials

Credential enrollment is outside the general renderer. Windows uses audited
native credential UI inside the daemon after owner-window qualification. Linux
uses a bounded pinentry Assuan exchange with a protected executable and cleared
environment. Keys never enter renderer, preload, Electron-main, argv, or
environment state. See
[ADR 0005](../decisions/0005-native-credential-enrollment.md).

### Presentation boundaries

- Deep links: closed `grok-desktop://open/v1/...` grammar parsed only in
  Electron main ([ADR 0012](../decisions/0012-versioned-desktop-deep-links.md)).
- External URLs: typed broker through preload/main; no renderer `href` /
  `window.open` / shell primitive
  ([ADR 0014](../decisions/0014-strict-external-url-broker.md)).

## Dependency rule

Domain code is deterministic and framework-free. Application use cases depend
on capability-focused ports. Adapters translate external contracts into those
ports. Composition roots are the only modules allowed to choose concrete
implementations.

Grok integration is deliberately not modeled as a generic provider interface.
Subscription ACP and the xAI API have different capability and trust models;
the capability resolver presents their supported behavior without forcing a
false lowest-common-denominator abstraction.

Expanded mapping to Clean Architecture and SOLID: [principles.md](principles.md).

## Authority and recovery

Every operation begins without tool authority. A grant is tied to a subject,
resource, action set, scope, and expiry. Chat and scheduled tasks never inherit
an interactive Work grant.

Run intent and approval are durable before a side effect starts. A completed
retry-safe read may be dispatched again only as a new recorded attempt. A
non-idempotent side effect without a durable result transitions to
`interrupted_needs_review` and waits for a human decision; it is never replayed
automatically.

An exact approval decision is committed with its run transition. Grant resumes
running; denial or expiry moves the run to paused so it cannot remain stranded
in `awaiting_approval`. Replay of the same command returns the same durable
outcome.

The privileged-operation foundation uses a closed kind, typed non-secret
target, immutable authority/idempotency metadata, digests, and a separately
identified record per dispatch attempt. See
[platform ADR 0003](../platform/adr/0003-durable-privileged-operation-journal.md).

There is still no production guest-control caller. Capability resolution keeps
`guestGrant=false`, and Work remains fail-closed in Limited Mode until the
complete Windows path is qualified.

Durable conversation, fork, artifact, and search contracts are summarized in
[protocol-and-persistence.md](protocol-and-persistence.md).

## Platform strategy

Windows 11 x64 and ARM64 are the first qualification targets. UI, domain,
provider, storage, and integration code is cross-platform. Execution,
computer-use, vault, notification, and update behavior is selected through
platform ports so macOS and Linux implementations can be added without changing
domain behavior.

Release packaging: [windows-release.md](../platform/windows-release.md).  
Threat model: [threat-model.md](../platform/threat-model.md).  
Cowork reference deltas: [claude-cowork-windows.md](../research/claude-cowork-windows.md).

## Related

| Doc | Use when |
|-----|----------|
| [principles.md](principles.md) | Guidelines, SOLID, clean code |
| [modules.md](modules.md) | Where to put code |
| [protocol-and-persistence.md](protocol-and-persistence.md) | IPC / schema chronicle |
| [decisions/](../decisions/README.md) | Why a contract changed |
| [implementation-status.md](../quality/implementation-status.md) | What is implemented now |
