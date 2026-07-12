# Linux packaging and release notes

- Status: engineering packaging path (not distribution-qualified)
- Related: [linux-ga.md](../quality/linux-ga.md), platform ADRs 0004–0007

## Package entry

From a Linux host with a prior desktop build and `grok-daemon` binary:

```sh
pnpm --filter @grok-desktop/desktop build
cargo build -p grok-daemon --release   # or use target/debug
pnpm package:linux -- --arch x64
```

Optional:

```sh
pnpm package:linux -- --arch x64 --daemon /path/to/grok-daemon --out /path/to/out
```

Outputs under `out/release/linux/<arch>/`:

- `unpacked/` — Electron app directory with embedded `resources/bin/grok-daemon`
- `grok-desktop.desktop` — includes `x-scheme-handler/grok-desktop`
- `linux-package.json` — layout record and daemon digest

## Grok Build host authentication

Subscription sign-in is owned by the **official Grok Build ACP client**. Grok
Desktop never embeds an unofficial OAuth client or imports browser cookies.

| Path | When | Requirements |
| --- | --- | --- |
| Development | Unpackaged Electron (`pnpm dev` / `pnpm dev:cdp`) | `cargo build -p grok-daemon --features debug-acp-descriptor`, official `grok` on `PATH` (or explicit `GROK_ACP_EXECUTABLE` / `VERSION` / `SHA256`). Electron main injects the descriptor only when `allowDevelopmentBinary` is true. |
| Product / package | Not yet staged by `package:linux` | A future packager must verify the signed catalog, publisher, target, executable identity, and digest before and after staging. Release daemons reject development overrides. |

Release daemons reject legacy `GROK_ACP_*` overrides. Development descriptors
are stripped for packaged launches.

## Runtime dependencies

- Official `grok` CLI for development Grok Build host authentication
- `pinentry` (or `pinentry-qt` / distro pinentry) for BYOK enrollment
- Secret Service / libsecret for vault persistence
- `xdg-desktop-portal` for artifact open on Linux
- For **Work** (not embedded in this package): KVM (`/dev/kvm`), the
  `linux-vm-service` privileged unit, and a signed virtio guest image

## Updates

In-app auto-update is not connected. Settings must remain honest about manual
channel updates until a signed updater ships.

## Isolation honesty

This package does **not** embed a hypervisor guest image. Work/Shell/MCP stay
unavailable until the Linux broker path qualifies. Never treat packaging success
as Work readiness. Host Grok Build authentication does not grant Work tools.
