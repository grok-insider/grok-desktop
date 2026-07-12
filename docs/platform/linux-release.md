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

## Runtime dependencies

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
as Work readiness.
