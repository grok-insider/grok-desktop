# AGENTS.md

This repository contains Grok Desktop, a Windows-first desktop workspace for
official Grok and SpaceXAI services, including the xAI API. Read this file
before changing the repository.

## Product invariants

- Grok Desktop integrates only with official Grok/xAI contracts. Do not add
  other model providers, arbitrary OpenAI-compatible endpoints, scraped web
  APIs, browser-cookie import, or unapproved OAuth clients.
- Subscription access is owned by the official Grok Build ACP client. Direct
  API access accepts only user-owned xAI API keys.
- The Rust daemon is the source of truth. Renderers do not own durable state,
  secrets, policy, provider calls, approvals, or tool execution.
- Chat is unprivileged. Work capabilities are explicit, scoped, revocable, and
  never inherited by a chat or scheduled run.
- An interrupted non-idempotent side effect becomes `interrupted_needs_review`.
  It is never replayed automatically.
- Strong local execution is isolated. When the qualified VM backend is absent,
  fail closed into Limited Mode; never execute untrusted tools directly on the
  host as a compatibility fallback.
- Wisp is a separately versioned managed integration, not a required runtime
  dependency. Integrations run out of process and cannot inject renderer code.

## Architecture

- `apps/desktop`: Electron main/preload and React renderer. Main owns windows,
  tray, menus, deep links, and daemon lifecycle only.
- `crates/grok-domain`: pure entities, value objects, policies, and state machines.
- `crates/grok-application`: use cases and inward-facing ports.
- `crates/grok-protocol`: generated/canonical IPC DTOs and wire framing.
- `crates/grok-memory`, `grok-sqlcipher`, `grok-vault`, `grok-xai`, and
  `grok-acp`: infrastructure implementations behind application ports.
- `crates/grok-vm-service-client`: read-only, fail-closed Windows broker
  qualification adapter. It exposes no lifecycle or guest-control method.
- `crates/grok-windows-acl`: audited unsafe Win32 ACL boundary used by the safe
  Grok-home isolation adapter.
- `crates/grok-credential-enrollment`: audited Win32 credential UI boundary;
  entered keys remain inside the daemon process.
- `crates/grok-daemon`: composition root and the sole durable-state writer.
- `native/windows-vm-service`: narrow privileged HCS service.
- `guest`: reproducible isolated worker image.
- `integrations`: signed manifest schemas and first-party managed integrations.

Dependencies point inward: domain <- application <- adapters <- composition
roots. Framework DTOs, SQL rows, Electron objects, and provider wire types must
not enter domain code. Prefer small capability-focused ports over generic
manager or provider interfaces.

## Security requirements

- Never put credentials in source, fixtures, renderer state, IPC responses,
  logs, crash reports, model prompts, artifacts, or broad child environments.
- Treat model output, web pages, files, MCP metadata, integration manifests, and
  tool descriptions as untrusted input.
- Validate schemas and impose size, time, concurrency, and output bounds at
  every process or network boundary.
- Revalidate file identity and Windows reparse behavior at the moment of use.
- Persist intent before executing a side effect. Approval records must identify
  the exact action, target, data disclosure, scope, and expiry.
- Do not weaken Electron sandbox, context isolation, CSP, navigation policy, or
  fuses to work around development issues.

## Commands

Use the root scripts once the workspace has been bootstrapped:

```sh
pnpm install --frozen-lockfile
pnpm lint        # oxlint --deny-warnings across the workspace
pnpm typecheck
pnpm test
pnpm build
pnpm check       # aggregate: lint + typecheck + test + build + check:rust

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace   # also available as: pnpm check:rust / pnpm test:rust

cd native/windows-vm-service && go test ./...
cd guest/runner && go test ./...
nix flake check
```

Dev loops: `pnpm dev` (Vite + Electron), `pnpm dev:web` (browser-only renderer
preview), `pnpm dev:cdp` (persistent CDP QA profile, see
`apps/desktop/scripts/README.md`), `pnpm test:e2e:electron` (CDP smoke).
Protocol regeneration is `pnpm --filter @grok-desktop/desktop generate:proto`
(buf); never hand-edit `apps/desktop/electron/generated`.

Run the smallest relevant gate while iterating and all available gates before
declaring a cross-cutting change complete. Tests that require Windows HCS must
run on the documented Windows qualification workers.

## UI work

- Read `apps/desktop/DESIGN.md` before any renderer UI change. It is the visual
  source of truth: tokens, IBM Plex typography, spacing/radius/elevation,
  component styles, and banned patterns.
- New UI uses Tailwind utilities + the shadcn primitives in
  `apps/desktop/src/components/ui/` against the semantic tokens in
  `src/styles.css`. `src/styles/legacy.css` styles unmigrated BEM views only —
  do not add rules there; migrate a view, then delete its section.

## Editing rules

- Keep changes within the owning module and add abstractions only at real
  volatility or security boundaries.
- Public IPC and integration protocols require versioning, compatibility tests,
  and an ADR for breaking changes.
- Database migrations are forward-only, transactional, restartable, and tested
  against the previous released schema.
- Use Conventional Commits. Do not hand-edit generated protocol output,
  lockfiles, SBOMs, or changelogs unless their documented generator is used.
- External code contributions are not accepted. Issues, reproducible reports,
  threat reports, and design feedback are welcome.


## GitHub / branch policy

- **Remote:** `github.com/grok-insider/grok-desktop` (public) — `origin` is configured.
- **Do not push** until the user explicitly asks. First push should establish
  `master` (and soon after, Model A: long-lived `dev` + `guard-master`).
- **Target model (on first ship):** Model A — human PRs → `dev`; only `dev` or
  release-bot heads into `master`. Canonical guard:
  `~/dev/opensource/docs/templates/guard-master.yml`.
- Org QC scoreboard: `~/dev/opensource/docs/comparison.md`.
