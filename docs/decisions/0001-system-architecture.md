# ADR 0001: Electron shell with a Rust system-of-record daemon

- Status: accepted
- Date: 2026-07-10

## Context

The product needs a rich, accessible workspace with consistent rendering on
Windows, durable work that survives UI crashes, strong process boundaries, and
later macOS/Linux support. Provider credentials and tool authority must not
exist in a web renderer.

## Decision

Use Electron and React for presentation, with sandboxed renderers and a narrow
preload. Use a separate Rust per-user daemon as the sole owner of domain state,
persistence, provider calls, approvals, integrations, scheduling, and worker
supervision. Communicate through a versioned local Protobuf protocol.

Use a separate, minimal Go service for privileged Windows HCS operations because
the maintained Windows container/virtualization primitives and operational
examples are strongest in that ecosystem. The service is not a second product
backend and exposes no arbitrary execution API.

## Consequences

The renderer remains replaceable and recoverable, but cross-process protocol
compatibility becomes a release requirement. Electron security releases need a
short qualification cycle. Native modules are kept out of Electron whenever a
daemon or sidecar boundary is available.

