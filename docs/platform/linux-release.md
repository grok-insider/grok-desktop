# Linux packaging and release notes

- Status: engineering packaging path (not distribution-qualified)
- Related: [linux-ga.md](../quality/linux-ga.md), platform ADRs 0004–0007

## Package entry

The Limited Mode package still accepts only the daemon. A product package may
add the official ACP component and privileged broker only as one complete,
verified input set.

```sh
pnpm --filter @grok-desktop/desktop build
cargo build -p grok-daemon --release   # or use target/debug
pnpm package:linux -- --arch x64
```

Optional:

```sh
pnpm package:linux -- --arch x64 --daemon /path/to/grok-daemon --out /path/to/out
```

For the product inputs, build the daemon with both public trust bindings, then
pass all related inputs together:

```sh
pnpm package:linux -- \
  --arch x64 \
  --daemon /path/to/trust-bound/grok-daemon \
  --acp-catalog /path/to/catalog.json \
  --acp-component /path/to/grok \
  --acp-trust-file /path/to/acp-public-keys.txt \
  --vm-service /path/to/grok-linux-vm-service \
  --daemon-uid 1000 \
  --service-group grok-desktop-broker
```

`GROK_ACP_CATALOG_TRUSTED_KEYS` and its
`grok-acp-catalog-trust-v1:<sha256-of-raw-value>` build binding must be embedded
in the daemon. The broker SHA-256 and its
`grok-linux-vm-service-trust-v1:<sha256-of-digest-text>` binding must likewise be
embedded through `GROK_LINUX_VM_SERVICE_SHA256` and
`GROK_LINUX_VM_SERVICE_TRUST_BINDING` when compiling the daemon. These values
are public trust metadata, not credentials. Packaging rejects a daemon that is
not bound to the exact staged inputs.

Outputs under `out/release/linux/<arch>/`:

- `unpacked/` — Electron app directory with embedded `resources/bin/grok-daemon`
- `unpacked/.../resources/bin/components/grok-acp/` — optional verified catalog
  plus byte-identical `bin/grok`
- `linux-service/` — optional `/usr/libexec`, systemd unit, and root-installed
  environment-file layout for `linux-vm-service`
- `unpacked/.../grok-desktop.desktop` — includes the protocol handler
- `linux-package.json` — layout record and daemon digest

Release inputs are opened without following links and retained while they are
hashed and copied. Device/inode/size/mode are revalidated, output creation is
exclusive, copying is bounded, and the signed catalog and executable digest are
checked again after staging. The vendor `grok` bytes are never rewritten or
first-party signed.

## Grok Build host authentication

Subscription sign-in is owned by the **official Grok Build ACP client**. Grok
Desktop never embeds an unofficial OAuth client or imports browser cookies.

| Path | When | Requirements |
| --- | --- | --- |
| Development | Unpackaged Electron (`pnpm dev` / `pnpm dev:cdp`) | `cargo build -p grok-daemon --features debug-acp-descriptor`, official `grok` on `PATH` (or explicit `GROK_ACP_EXECUTABLE` / `VERSION` / `SHA256`). Electron main injects the descriptor only when `allowDevelopmentBinary` is true. |
| Product / package | Optional complete `--acp-*` input set | Signed catalog, pinned public trust, `xAI` publisher, semantic version, Linux architecture, retained executable identity, size, and digest are verified before and after byte-identical staging. Release daemons reject development overrides. |

Release daemons reject legacy `GROK_ACP_*` overrides. Development descriptors
are stripped for packaged launches.

## Runtime dependencies

- Official `grok` CLI for development Grok Build host authentication
- `pinentry` (or `pinentry-qt` / distro pinentry) for BYOK enrollment
- Secret Service / libsecret for vault persistence
- `xdg-desktop-portal` for artifact open on Linux
- For **Work** (not embedded in this package): KVM (`/dev/kvm`), the
  `linux-vm-service` privileged unit, and a signed virtio guest image

## Privileged broker installation policy

Install `linux-service/usr/libexec/grok-desktop/grok-linux-vm-service` as
root-owned and non-writable by group/other. Install the unit and environment
file at their staged absolute paths, create the configured service group, and
add only the intended desktop account to it. `--daemon-uid` is written as the
exact `SO_PEERCRED` UID; it is not inferred at runtime. The fixed daemon path is
`/opt/grok-desktop/resources/bin/grok-daemon`.

The systemd policy fixes a root-owned `0750` runtime directory, `0660` Unix
socket, root service user, explicit service group, private state directory,
closed `/dev/kvm` device policy, read-only system, and narrow writable paths.
The broker additionally locks the socket namespace, rejects links and unsafe
owners/modes, and binds accepted peers to UID plus executable device/inode.

## Updates

In-app auto-update is not connected. Settings must remain honest about manual
channel updates until a signed updater ships.

## Isolation honesty

This package does **not** fabricate a production publisher key or embed a
qualified hypervisor guest image. The production probe uses only the fixed
root-owned socket and broker path, requires an embedded broker digest binding,
checks `SO_PEERCRED` and the running executable, and rejects service-supplied
qualification booleans without those local identity checks. Environment socket
discovery is debug-feature-only and can never qualify Work.

The broker currently reports signed guest, selected image, broker package, and
hardware qualification evidence as false. Therefore Work/Shell/MCP remain
unavailable even when packaging and socket tests pass. Enabling them still
requires production trust roots, signed guest/catalog evidence, and documented
KVM hardware qualification. Host Grok Build authentication never grants Work
tools.
