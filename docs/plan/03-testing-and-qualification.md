# Testing and qualification

## Evidence rules

Record command, commit, date, platform, exit result, and skipped-gate reason.
Compilation and mocks prove local behavior, not production identity, billing,
hypervisor isolation, native tray appearance, or release signing.

## Per-change gates

- TypeScript: focused Vitest, then affected typecheck and lint.
- Rust: focused crate tests, `cargo fmt --all --check`, and focused Clippy.
- Go: affected service or guest package tests.
- Protocol: regenerate with the documented command and test rejection of every
  prior epoch. Never hand-edit generated Electron protocol output.
- Persistence: memory/SQL parity, previous-schema migration, rollback, restart,
  corruption, and fault-injection tests.
- Packaging: deterministic fixture catalog verification and unpacked-layout
  identity checks.

## Wisp headless UI procedure

Use the `wisp-debugging` workflow and Wisp nested GUI sandbox exclusively while
the user is using the computer:

1. Launch `pnpm dev:web` inside the nested environment.
2. Never open Electron or touch the real display, cursor, or any host workspace.
3. Test 1440×900, 2560×1440, and 640×900.
4. Cover Setup, Settings, Chat, Automations, Extensions/Wisp, Library, loading,
   empty, failure, and unavailable states.
5. Verify keyboard navigation, accessible names/status, focus restoration,
   dialog trapping/Escape, reduced motion, clipping, and overflow.
6. Refresh grounded element identifiers before each action and capture evidence
   after material state transitions.
7. Close the nested session, stop preview servers, and verify no child process
   or test port remains.

Native tray and Electron lifecycle behavior use unit/integration suites during
headless work. Native visual inspection is an external isolated qualification.

## Required scenario suites

- Scheduler: DST gap/fold, timezone changes, stale fence, concurrent workers,
  exact/conflicting keys, crash at every write, restart of every state, no
  duplicate binding, no dispatch in Limited Mode, clean shutdown.
- Wisp: manifest/signing-byte mismatch, publisher/version/file tamper, links,
  identity swap, oversized inputs, interrupted stage/publish/remove, revision
  conflict/overflow, restart, rollback, unrelated daemon startup.
- ACP/Linux: unsafe executable/catalog, architecture mismatch, mutated identity,
  wrong peer, socket permissions, oversized/malformed frames, timeout,
  concurrency exhaustion, stale socket, broker restart, no host fallback.
- Chat: existing xAI-key enrollment, model discovery, streamed terminal
  outcomes, retry/fork lineage, cancellation, restart recovery, no tool or Work
  inheritance. The SuperGrok rail adds separate OAuth, refresh, revocation,
  no-fallback, and billing-attribution suites only after contract approval.

## Full local gate

Before a milestone is called locally complete:

```sh
pnpm check
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
(cd native/windows-vm-service && go test ./...)
(cd native/linux-vm-service && go test ./...)
(cd guest/runner && go test ./...)
nix flake check
```

Also require clean protocol regeneration, Linux package-layout verification,
previous-schema migration/restart evidence, and the Wisp headless regression.
Unavailable platform tools are recorded as external gates, not passes.
