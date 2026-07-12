# Local development

How to bootstrap and run Grok Desktop from source. There are no packaged public
releases yet; source is the only supported run mode.

## Prerequisites

- Node.js >= 22.22 with corepack enabled (repo pins `pnpm@10.33.2`)
- Rust 1.95+ (workspace edition 2024)
- Go, for `native/windows-vm-service` and `guest/runner`
- Nix (optional), for the reproducible guest image and `nix flake check`

## Install

From the repository root:

```sh
pnpm install --frozen-lockfile
```

## Dev loops

| Command | Purpose |
|---------|---------|
| `pnpm dev` | Vite + Electron development shell |
| `pnpm dev:web` | Browser-only renderer preview (explicit sample data; no daemon bridge) |
| `pnpm dev:cdp` | Persistent QA profile with Chrome DevTools Protocol |

CDP launcher detail and flags:
[apps/desktop/scripts/README.md](../../apps/desktop/scripts/README.md).

Typical QA profile:

```sh
pnpm dev:cdp -- --profile qa-local --port 9250
```

Opening the renderer without Electron’s preload bridge **or** the explicit
`pnpm dev:web` path fails closed instead of loading interactive sample data.

## Quality gates

```sh
pnpm lint        # oxlint --deny-warnings
pnpm typecheck
pnpm test
pnpm build
pnpm check       # lint + typecheck + test + build + check:rust

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace   # also: pnpm check:rust / pnpm test:rust

cd native/windows-vm-service && go test ./...
cd guest/runner && go test ./...
nix flake check   # optional full flake
```

Electron CDP smoke (daemon already on the QA profile):

```sh
pnpm test:e2e:electron -- --port 9250
```

## Linux graphics backend

Electron selects its startup backend before creating a window. Pure Wayland and
pure X11 sessions use their available backend. When both Wayland and XWayland
are present, Mesa-class systems prefer native Wayland while detected NVIDIA
systems prefer XWayland to avoid incompatible DMA-BUF/EGL imports. A GPU crash
before the first usable window causes one software-rendered restart; it never
loops or changes the renderer sandbox, context isolation, CSP, or web security.

For driver diagnosis, override the automatic policy explicitly:

```sh
pnpm dev -- --grok-graphics-backend=auto
pnpm dev -- --grok-graphics-backend=wayland
pnpm dev -- --grok-graphics-backend=x11
pnpm dev -- --grok-graphics-backend=software
```

Packaged Electron accepts the same `--grok-graphics-backend` values. Invalid or
conflicting values are ignored in favor of automatic selection.

Windows packaging: `pnpm package:windows`. HCS-dependent tests require the
documented Windows qualification workers. Release matrix:
[release-qualification.md](../quality/release-qualification.md). Packaging
layout: [windows-release.md](../platform/windows-release.md).

## Protocol regeneration

```sh
pnpm --filter @grok-desktop/desktop generate:proto
```

Never hand-edit `apps/desktop/electron/generated`. Public IPC changes need
versioning, compatibility tests, and an ADR
([decisions/README.md](../decisions/README.md)).

## Iteration policy

- Run the **smallest relevant gate** while iterating.
- Run **all available gates** before declaring a cross-cutting change complete.
- Do not weaken Electron sandbox, context isolation, CSP, navigation policy, or
  fuses to work around development issues.

## Next

- [Debugging and QA](debugging-and-qa.md) — CDP, e2e, Wisp, Hyprland
- [Coding guidelines](coding-guidelines.md)
- [Engineering principles](../architecture/principles.md)
