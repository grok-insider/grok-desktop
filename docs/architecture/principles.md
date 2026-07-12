# Engineering principles

This document is the guidelines of record for Grok Desktop architecture and
coding style. It names how the repository maps to Clean Architecture, SOLID,
and clean-code practice, and what this project deliberately does **not** do.

Read this before changing domain, application, protocol, or security-sensitive
code. Product invariants in [AGENTS.md](../../AGENTS.md) still apply on every
change.

## When rules conflict

Priority order:

1. **Security and authority** — secrets stay in the daemon; untrusted input is
   bounded; fail closed; no silent privilege expansion.
2. **Protocol and recovery compatibility** — versioned IPC, forward-only
   migrations, durable intent before side effects, no automatic replay of
   non-idempotent work.
3. **Layering and SOLID** — dependency rule, small ports, composition at the
   root.
4. **Local elegance** — shorter code and fewer types only after the above.

Security and authority boundaries outrank abstract purity.

## Product invariants (condensed)

- Official Grok / xAI contracts only. No other model providers, scraped web
  APIs, cookie import, or unapproved OAuth clients.
- Rust daemon is the system of record. Renderers do not own durable state,
  secrets, policy, provider calls, approvals, or tool execution.
- Chat is unprivileged. Work capabilities are explicit, scoped, revocable, and
  never inherited by a chat or scheduled run.
- Interrupted non-idempotent side effects become `interrupted_needs_review` and
  are never replayed automatically.
- Strong local execution is isolated. Without a qualified VM backend, fail
  closed into Limited Mode.
- Wisp is a separately versioned managed integration, not a required runtime
  dependency. Integrations run out of process and cannot inject renderer code.

## Clean Architecture / ports and adapters

The daemon side follows Clean Architecture (hexagonal / ports-and-adapters)
with an extra hard process boundary for presentation.

```text
Presentation   apps/desktop (Electron renderer, preload, main)
      |        versioned local Protobuf IPC only
Composition    crates/grok-daemon  (wires concrete adapters)
      |
Application    crates/grok-application  (use cases + ports/traits)
      |
Domain         crates/grok-domain  (entities, value objects, policies, FSMs)
      ^
Adapters       grok-sqlcipher, grok-vault, grok-xai, grok-acp, grok-memory,
               grok-vm-service-client, grok-credential-enrollment, …
```

| Layer | Responsibility | Must not |
|-------|----------------|----------|
| Domain | Deterministic product rules | Import SQL, Electron, provider wire types, IPC DTOs |
| Application | Use cases; define ports | Choose OS/crypto/provider concrete types |
| Adapters | Translate external contracts into ports | Own durable policy or secret policy |
| Composition (`grok-daemon`) | Construct graph; sole durable-state writer | Leak secrets into IPC responses |
| Presentation | UI, windowing, daemon lifecycle supervision | Own domain authority or credentials |

### Dependency rule

Dependencies point **inward**: domain ← application ← adapters ← composition
roots. Framework DTOs, SQL rows, Electron objects, and provider wire types must
not enter domain code.

Application use cases depend on **capability-focused ports** (traits). Adapters
implement those ports. Only composition roots choose concrete implementations.

Module ownership detail: [modules.md](modules.md). System process model:
[overview.md](overview.md).

### Deliberate non-textbook choices

These are design decisions, not layering failures:

- **Process isolation first.** The renderer is untrusted presentation. Classic
  in-process Clean Architecture UI layers are insufficient for this threat
  model.
- **No generic multi-provider plugin interface.** Subscription ACP and the xAI
  API have different trust and capability models. A lowest-common-denominator
  “LLM provider” trait would be a false abstraction (see overview dependency
  rule and [ADR 0002](../decisions/0002-grok-only-integrations.md)).
- **Privileged Windows HCS** lives in a narrow Go service and guest image,
  reached only through fail-closed ports—not from the renderer and not as a
  second product backend.
- **Wisp** is a managed out-of-process integration, not a domain dependency.
  Product contract: [integrations/first-party/wisp/ADAPTER.md](../../integrations/first-party/wisp/ADAPTER.md).
  Developer QA tooling that also uses Wisp is separate; see
  [debugging-and-qa.md](../development/debugging-and-qa.md).

## SOLID

| Principle | Application in this repo |
|-----------|--------------------------|
| **S**ingle responsibility | Crates and ports split by capability (for example `ArtifactStore` vs `ArtifactOpener` vs `ArtifactContentRetention`). Domain modules own one aggregate or policy area. Electron main owns windows/tray/deep links/daemon lifecycle only. |
| **O**pen/closed | New platform or storage behavior arrives as adapters behind existing ports. Domain rules stay free of wire formats. Public IPC changes are versioned and ADR’d rather than silently extended. |
| **L**iskov substitution | Adapters must honor port contracts: fail closed when unqualified, never log secrets, never widen authority. Limited Mode is a valid substitution when the VM backend is absent—not a silent host-exec fallback. |
| **I**nterface segregation | Prefer small capability-focused ports over generic manager or provider interfaces ([AGENTS.md](../../AGENTS.md)). |
| **D**ependency inversion | Application code depends on traits; `grok-daemon` injects implementations (`sqlcipher`, vault, xAI, ACP, enrollment, isolation probe). |

### Rejected patterns

- God “manager” or “service locator” objects that mix persistence, policy, and
  UI.
- Renderer-owned mutation of durable conversation, artifact, or run state.
- Adding a second model provider “for testing” behind the same port.
- Weakening Electron sandbox, CSP, or context isolation to ease local debug.
- Automatic replay of non-idempotent side effects after interruption.

## Clean code and working style

- **Own the module.** Keep changes inside the owning crate or app package. Add
  abstractions only at real volatility or security boundaries.
- **Bounds everywhere.** Schema validation and size, time, concurrency, and
  output limits at every process or network boundary.
- **Intent before effect.** Persist intent and approval identity before side
  effects. Approval records identify action, target, disclosure, scope, and
  expiry.
- **Fail closed.** Prefer unavailable or review-required outcomes over inventing
  success, zero usage, or host execution.
- **Secrets never travel.** No credentials in source, fixtures, renderer state,
  IPC responses, logs, crash reports, model prompts, artifacts, or broad child
  environments.
- **Untrusted input.** Model output, web pages, files, MCP metadata, manifests,
  and tool descriptions are untrusted.
- **Smallest gate, then full gate.** Run the smallest relevant check while
  iterating; run all available gates before declaring a cross-cutting change
  complete (`pnpm check`, Rust `clippy -D warnings`, Go tests where touched).
- **No hand-edits of generated artifacts.** Protocol output, lockfiles, SBOMs,
  and changelogs use their documented generators.
- **Conventional Commits.**
- **UI.** Read [apps/desktop/DESIGN.md](../../apps/desktop/DESIGN.md). New UI
  uses Tailwind + shadcn primitives and semantic tokens. Do not grow
  `legacy.css`; migrate then delete.

Day-to-day checklists: [coding-guidelines.md](../development/coding-guidelines.md).

## Trust model (one page)

| Component | Trust |
|-----------|--------|
| Electron renderer, model output, files, web content, MCP servers, managed add-ons, browser pages, VM workers | Untrusted |
| Rust daemon, platform vault, signed updater, minimal Windows VM service | Trusted components |
| Approvals | Improve user control; not a substitute for containment |

Full platform threats and controls:
[threat-model.md](../platform/threat-model.md). Reporting:
[SECURITY.md](../../SECURITY.md).

## Related docs

- [Architecture overview](overview.md)
- [Module map](modules.md)
- [Protocol and persistence chronicle](protocol-and-persistence.md)
- [ADR index](../decisions/README.md)
- [Local development](../development/local-development.md)
- [Debugging and QA](../development/debugging-and-qa.md)
