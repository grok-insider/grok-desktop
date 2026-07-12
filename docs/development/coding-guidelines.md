# Coding guidelines

Practical rules for humans and AI agents changing this repository. Architecture
rationale: [principles.md](../architecture/principles.md). Module ownership:
[modules.md](../architecture/modules.md).

## Before you edit

1. Read [AGENTS.md](../../AGENTS.md) product invariants.
2. Confirm the owning module ([modules.md](../architecture/modules.md)).
3. For UI, read [apps/desktop/DESIGN.md](../../apps/desktop/DESIGN.md).
4. For public IPC, integrations, or recovery semantics, find the ADR first
   ([decisions/README.md](../decisions/README.md)).

## Change shape

- Keep changes inside the owning module.
- Add abstractions only at real volatility or security boundaries.
- Prefer small capability-focused ports over generic managers.
- Do not introduce other model providers or OpenAI-compatible escape hatches.
- Do not move durable authority into the renderer “for convenience.”
- Do not hand-edit generated protocol output, lockfiles, SBOMs, or changelogs.

## Security checklist

- [ ] No credentials in source, fixtures, renderer state, IPC responses, logs,
      crash reports, model prompts, artifacts, or broad child environments.
- [ ] Untrusted input (model output, web, files, MCP, manifests, tools) is
      validated and bounded.
- [ ] Side effects persist intent first; non-idempotent interruption stays
      `interrupted_needs_review` (never auto-replayed).
- [ ] Electron sandbox, context isolation, CSP, navigation policy, and fuses
      are not weakened for local development.
- [ ] File identity / reparse behavior revalidated at the moment of use when
      touching filesystem adapters.

## UI checklist

- [ ] Tokens and components follow DESIGN.md (IBM Plex, semantic colors, focus).
- [ ] New UI uses Tailwind utilities + `apps/desktop/src/components/ui/`.
- [ ] No new rules in `src/styles/legacy.css`; migrate then delete legacy BEM.
- [ ] Accessibility: visible focus, contrast, reduced motion respected.

## Protocol and persistence checklist

- [ ] Public IPC change is versioned; old epochs rejected as designed.
- [ ] Compatibility tests updated.
- [ ] Breaking or authority-shifting change has an ADR.
- [ ] Migrations are forward-only, transactional, restartable, and tested
      against the previous released schema.
- [ ] Regenerated clients via
      `pnpm --filter @grok-desktop/desktop generate:proto`.

## Testing

| Layer | Typical command |
|-------|-----------------|
| JS/TS workspace | `pnpm test`, `pnpm typecheck`, `pnpm lint` |
| Rust workspace | `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` |
| Go services | `go test ./...` under `native/windows-vm-service` or `guest/runner` |
| Electron smoke | `pnpm dev:cdp` + `pnpm test:e2e:electron` (see [debugging-and-qa.md](debugging-and-qa.md)) |
| Aggregate | `pnpm check` |

Run the smallest relevant gate while iterating. Before claiming a cross-cutting
change complete, run all available gates for the areas touched. Windows HCS
paths require qualification workers.

## Commits

Use Conventional Commits (for example `feat:`, `fix:`, `docs:`, `refactor:`,
`test:`, `chore:`).

Do not force-push shared history or amend published commits unless explicitly
requested by a maintainer.

## Definition of done

A change is done when:

1. Behavior matches product invariants and relevant ADRs.
2. Relevant automated gates pass.
3. No new secret or authority leakage paths were introduced.
4. Docs/ADRs updated when public contracts or recovery semantics changed.
5. UI changes match DESIGN.md and do not regress accessibility contracts covered
   by the CDP probe when those surfaces are in scope.

## Out of scope for drive-by edits

- Opening contribution policy without an explicit product decision.
- Rewriting history for cosmetic reasons.
- “Temporary” host execution of untrusted tools when the VM backend is missing.
