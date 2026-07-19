# Linux packaging and release notes

- Status: engineering packaging path (not distribution-qualified)
- Related: [linux-ga.md](../quality/linux-ga.md), platform ADRs 0004–0007

## NixOS / Cachix

Release candidate Linux builds push the exact `portableLinuxRuntime` store path
to the `grok-insider` Cachix binary cache and publish
`out/release/linux/x64/nix-portable-runtime.json` beside the AppImage. A clean
NixOS client can realize that path without rebuilding:

```sh
# Prefer the project cache, then cache.nixos.org
export NIX_CONFIG="extra-substituters = https://grok-insider.cachix.org
extra-trusted-public-keys = grok-insider.cachix.org-1:ZxLVOxJ1CjdY3vQl1I99qCtwNZwIU4+/QwqSvntB/5w="
store_path="$(jq -r .storePath nix-portable-runtime.json)"
nix-store --realise "$store_path"  # or: nix build --store auto "$store_path"
```

Against a tagged source, the same attribute is:

```sh
nix build "github:grok-insider/grok-desktop/<tag>#portableLinuxRuntime" \
  --option extra-substituters https://grok-insider.cachix.org \
  --option extra-trusted-public-keys 'grok-insider.cachix.org-1:ZxLVOxJ1CjdY3vQl1I99qCtwNZwIU4+/QwqSvntB/5w=' \
  --option max-jobs 0
```

`--option max-jobs 0` forces substitution-only for that evaluation when the
cache holds the path; a miss fails closed instead of compiling locally.

## Package entry

The Limited Mode package still accepts only the daemon. A product package may
add the official ACP component and privileged broker only as one complete,
verified input set.

```sh
repo_root="$(git rev-parse --show-toplevel)"
runtime="$repo_root/result-portable-linux-runtime"
pnpm --filter @grok-desktop/desktop build
nix build --out-link "$runtime" "$repo_root#portableLinuxRuntime"
pnpm package:linux -- --arch x64 \
  --daemon "$runtime/bin/grok-daemon" \
  --host-tools-helper "$runtime/bin/grok-host-tools-mcp" \
  --appimagetool /path/to/pinned/appimagetool-x86_64.AppImage \
  --appimagetool-sha256 <lowercase-sha256> \
  --appimageupdatetool /path/to/pinned/appimageupdatetool-x86_64.AppImage \
  --appimageupdatetool-sha256 <lowercase-sha256> \
  --update-trust-file /path/to/stable-update-public-keys.json
```

Optional:

```sh
pnpm package:linux -- --arch x64 --daemon /path/to/grok-daemon --out /path/to/out \
  --host-tools-helper /path/to/grok-host-tools-mcp \
  --appimagetool /path/to/pinned/appimagetool-x86_64.AppImage \
  --appimagetool-sha256 <lowercase-sha256> \
  --appimageupdatetool /path/to/pinned/appimageupdatetool-x86_64.AppImage \
  --appimageupdatetool-sha256 <lowercase-sha256> \
  --update-trust-file /path/to/stable-update-public-keys.json
```

For the product inputs, build the daemon with both public trust bindings, then
pass all related inputs together:

```sh
pnpm package:linux -- \
  --arch x64 \
  --daemon /path/to/trust-bound/grok-daemon \
  --host-tools-helper /path/to/grok-host-tools-mcp \
  --acp-catalog /path/to/catalog.json \
  --acp-component /path/to/grok \
  --acp-trust-file /path/to/acp-public-keys.txt \
  --vm-service /path/to/grok-linux-vm-service \
  --daemon-uid 1000 \
  --service-group grok-desktop-broker \
  --appimagetool /path/to/pinned/appimagetool-x86_64.AppImage \
  --appimagetool-sha256 <lowercase-sha256> \
  --appimageupdatetool /path/to/pinned/appimageupdatetool-x86_64.AppImage \
  --appimageupdatetool-sha256 <lowercase-sha256> \
  --update-trust-file /path/to/stable-update-public-keys.json
```

The package command verifies both explicitly supplied AppImage tool digests
and the bounded Ed25519 stable-channel public trust set,
preserves the already-verified Electron layout, and emits a stable AppImage plus
its `.zsync` differential-update metadata. Release workers pin the tool bytes;
the packaging command never downloads or discovers a tool at runtime.

Public AppImages accept only self-contained ELF64 builds of the first-party
`grok-daemon` and `grok-host-tools-mcp` runtimes. The flake's
`portableLinuxRuntime` output cross-builds both executables against static musl.
Packaging and final artifact verification independently parse their ELF
program headers and reject `PT_INTERP`, dynamic dependency/control tags,
including `DT_NEEDED`, `DT_RPATH`, `DT_RUNPATH`, audit, filter, auxiliary, and
config tags, Nix-store loaders, and binaries coupled to the release worker's
glibc. A dependency-free `PT_DYNAMIC` is accepted only on `ET_DYN` executables
that declare `DF_1_PIE`, preserving ASLR without misclassifying a shared object
or fixed-address executable. Rewriting an ELF interpreter or copying a
build-host libc into the AppImage is not an accepted portability mechanism.

The v0.0.10 qualification run established this as a release gate: both
first-party Rust executables carried an unbundled `/nix/store/.../ld-linux`
`PT_INTERP`, so they failed as soon as `/nix` was absent. Portable native
runtime inspection is therefore required once before packaging and again from
the extracted final AppImage; a digest match alone is insufficient.

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
- `linux-package.json` — v2 layout record, daemon digest, and verified Electron fuse state

Release inputs are opened without following links and retained while they are
hashed and copied. Device/inode/size/mode are revalidated, output creation is
exclusive, copying is bounded, and the signed catalog and executable digest are
checked again after staging. The vendor `grok` bytes are never rewritten or
first-party signed.

Before AppImage assembly, packaging applies the shared Electron fuse policy and
re-reads every fuse from both the packaged executable and its AppDir copy. The
release workflow then runs `release:verify-linux-artifact` before upload: it
hashes the final AppImage against the v2 record, extracts it without a FUSE
mount, re-reads the embedded Electron fuse wire, and verifies the embedded
daemon, Host Tools helper, update helper, and staged official Grok component
against their recorded or tracked digests. The verifier also confirms that the
two first-party native runtimes remain static ELF executables after extraction.
It binds the `.zsync` bytes by SHA-256 and checks that their bounded header
names the exact AppImage length and SHA-1 required by the zsync format.
`EnableEmbeddedAsarIntegrityValidation` is kept aligned with the cross-platform
policy, but Electron currently enforces embedded ASAR integrity only on macOS
and Windows; Linux relies on the remaining fuse restrictions plus signed update
metadata and exact artifact verification. See the
[Electron fuse documentation](https://www.electronjs.org/docs/latest/tutorial/fuses).

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
- An FHS-compatible runtime providing the baseline AppImageUpdate libraries
  (`libstdc++`, zlib, and libgpg-error) for in-place AppImage updates. NixOS
  users should use a distribution package/update service until a qualified Nix
  package ships; the raw upstream update helper fails closed there.
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

Public AppImages embed the canonical stable GitHub `.zsync` location and a
digest-pinned AppImageUpdate helper. Packaged launches from an absolute
`APPIMAGE` path check the stable channel shortly after startup and every six
hours. A successful differential update replaces only that AppImage, then
Settings offers an explicit restart. Development, extracted, package-manager,
and otherwise unsupported installs remain read-only and report that status
honestly.

The helper receives only a narrow non-secret environment, fixed arguments, and
the current AppImage path. Packaging verifies its pinned release digest before
embedding it. Release publication must upload the stable `.AppImage` and its
matching `.AppImage.zsync` asset together; until those assets exist, checks
fail closed without changing the current executable.

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
