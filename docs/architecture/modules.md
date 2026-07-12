# Module map

Ownership and dependency directions for the main packages. Prefer changing code
inside the owning module. Full layering rules live in
[principles.md](principles.md).

## Dependency direction

```text
domain  ←  application  ←  adapters  ←  composition (grok-daemon)
                ↑
         presentation (apps/desktop) only via versioned IPC
```

- Domain depends on nothing in this workspace.
- Application may depend on domain only (plus pure utilities).
- Adapters may depend on application ports and domain types; they must not
  pull domain *policy* into framework types.
- `grok-daemon` is the composition root and sole durable-state writer.
- `apps/desktop` talks to the daemon through `grok-protocol` IPC only for
  product authority. Electron main supervises the daemon process; it does not
  interpret domain state.

## Rust crates

| Crate | Role | May depend on | Must not |
|-------|------|---------------|----------|
| `grok-domain` | Pure entities, value objects, policies, state machines | External pure crates only | SQL, Electron, provider SDKs, IPC DTOs, OS vault APIs |
| `grok-application` | Use cases and inward-facing ports (traits) | `grok-domain` | Concrete SQLCipher, Win32, Electron, raw xAI wire types as domain truth |
| `grok-protocol` | Canonical IPC DTOs, framing, validation | Generated/protobuf stack as needed | Domain business rules living only here |
| `grok-memory` | In-memory port implementations (tests, limited runtime) | application ports / domain | Become the production system of record |
| `grok-sqlcipher` | Encrypted durable stores behind ports | application ports / domain | Leak paths or secrets into public IPC |
| `grok-vault` | OS secret-storage adapter | application ports | Put key material in logs or IPC |
| `grok-xai` | Official xAI API adapter | application ports | Generic multi-provider façade |
| `grok-acp` | Official Grok Build ACP client integration | application ports | Custom OAuth or scraped sessions |
| `grok-vm-service-client` | Read-only, fail-closed Windows broker qualification | application isolation port | Lifecycle or guest-control methods |
| `grok-windows-acl` | Audited unsafe Win32 ACL boundary | used by safe home-isolation adapter | Be called from the renderer |
| `grok-credential-enrollment` | Audited credential UI / pinentry boundary | application enrollment ports | Return key bytes to Electron |
| `grok-artifact-storage` | Platform private content storage (e.g. Linux fd path) | application content ports | Expose storage paths on public IPC |
| `grok-daemon` | Composition root, IPC server, startup recovery | all adapters as wiring | Move authority into the renderer |

## Desktop application (`apps/desktop`)

| Surface | Owns | Must not |
|---------|------|----------|
| Renderer (React) | Presentation, local UI state, calling typed IPC | Secrets, durable mutation authority, tool execution |
| Preload | Narrow validated bridge | Broad Node APIs or secret handoff |
| Electron main | Windows, tray, menus, deep links, daemon lifecycle, external URL broker, file chooser handoff | Domain interpretation, credential storage |
| Generated protocol (`electron/generated`) | buf output | Hand edits — regenerate with documented command |

UI visual source of truth: [DESIGN.md](../../apps/desktop/DESIGN.md).

## Native and guest

| Path | Role |
|------|------|
| `native/windows-vm-service` | Narrow privileged HCS service (Go). Not a second product backend. No arbitrary execution API. |
| `guest/` | Reproducible isolated worker image and runner. |
| `integrations/` | Signed manifest schemas and first-party managed integrations. |
| `integrations/first-party/wisp/` | Reference managed Wisp adapter (product computer-use). Not a required desktop dependency. |

Platform guest ADRs: [docs/platform/adr/](../platform/adr/).

## Protocol generation

```sh
pnpm --filter @grok-desktop/desktop generate:proto
```

Never hand-edit `apps/desktop/electron/generated`. Breaking public IPC or
integration contracts requires versioning, compatibility tests, and an ADR
([decisions/README.md](../decisions/README.md)).

## Where to put new code

| If you are adding… | Prefer |
|--------------------|--------|
| A pure business rule or state machine | `grok-domain` |
| A use case orchestrating ports | `grok-application` |
| SQL / OS / network / provider implementation | New or existing adapter crate |
| Wiring / startup recovery / IPC dispatch | `grok-daemon` |
| UI view or component | `apps/desktop/src` per DESIGN.md |
| Privileged Windows virtualization | `native/windows-vm-service` + ports only |
| Managed add-on contract | `integrations/` with signed manifest discipline |
