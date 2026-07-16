# Windows release pipeline

Windows releases are assembled only on ephemeral, access-controlled Windows 11
workers. The public beta/core train ships an intentionally unsigned x64 NSIS
installer with Chat and explicitly enrolled Host Tools Work. Windows may show
Unknown Publisher or Microsoft Defender SmartScreen warnings. ARM64 and the
signed MSIX isolated-work train remain deferred until their native, guest,
identity, certificate, and service inputs are qualified.

## Public core package

The public core job constructs its release input tree on the trusted worker:

```text
windows-core/x64/
└── bin/
    ├── grok-daemon.exe
    ├── grok-host-tools-mcp.exe
    └── components/grok-acp/
        ├── pinned-component.json
        └── bin/grok.exe
```

The daemon and Host Tools helper are built from the tagged source. The daemon
contains the domain-separated SHA-256 binding of the tracked Windows x64
manifest. The job downloads `grok.exe` only from the exact `https://x.ai/cli/`
URL in that manifest, verifies its size and SHA-256 digest, and preserves its
bytes through packaging. The vendor executable is not re-signed or otherwise
modified.

Packaging rejects extra files, links, target mismatches, a daemon without the
exact manifest binding, or any component byte mismatch. The release record
labels the package `core-host-tools-beta` and lists isolated Work, media,
browser automation, and scheduled Work as deferred capabilities. This avoids
shipping placeholder guest/service inputs or claiming isolation that the
package does not contain.

The tracked pin and package release record are published beside the NSIS `.exe`
as public provenance evidence. Source pinning is governed by ADR 0033. The
installer is unsigned, while its exact URL, size, digest, version, platform,
architecture, and artifact kind remain authorized by the signed update
manifest.

## Active core NSIS packaging

The active Windows environment supplies qualified Cargo, Rust, MSVC, and cache
paths, bounded public update trust, and the exact xAI provenance and
redistribution evidence IDs. It supplies no
PFX, certificate password, MSIX identity, SignTool path, signer thumbprint, or
timestamp endpoint. Packaging rejects ambient Electron certificate variables
instead of silently signing with runner state.

Electron Packager creates and hardens the application directory before the
pinned `electron-builder` NSIS target wraps it. The release is one-click,
per-user, and does not request elevation. Its canonical artifact name is
`GrokDesktop-<channel>-x64.exe`; the application ID is
`com.grokinsider.grokdesktop`. Installation creates Start menu and desktop
shortcuts, preserves application data on uninstall, and owns only its
current-user `grok-desktop` protocol registration.

The release job verifies that the final installer is not Authenticode-signed,
records `codeSigning: unsigned` in the package record, publishes SHA-256
checksums, and requests a GitHub build attestation. Release notes identify the
expected Windows warning and link the checksum, attestation, and immutable
source tag. Signed update metadata is still mandatory: the updater accepts only
the canonical GitHub release URL and exact manifest-authorized `.exe`, then
revalidates the regular file, size, and SHA-256 immediately before direct
execution without a shell.

The active release command is:

```powershell
pnpm --filter @grok-desktop/desktop package:windows-core `
  --arch x64 --channel beta --stage $env:CORE_STAGE
```

Electron fuses, renderer sandboxing, the private `grok-desktop://app` renderer
origin, production navigation policy, and DevTools restrictions are identical
to the deferred package's hardening contract below.

## Qualified isolated MSIX package (deferred)

Everything in this section describes a future isolated/enterprise train. None
of its MSIX identity, Authenticode, packaged-service, guest-image, elevation, or
certificate prerequisites block the active public core NSIS release.

### Package contents

The full isolated train retains the following signed input contract for each
architecture once qualification resumes:

```text
release-inputs/windows/<x64|arm64>/
├── bin/grok-daemon.exe
├── bin/components/grok-acp/catalog.json
├── bin/components/grok-acp/bin/grok.exe
├── service/grok-vm-service.exe
├── guest/grok-guest.vhdx
├── guest/grok-guest.vhdx.sha256
├── catalog/components.json
├── catalog/integrations.json
└── release-inputs.json
```

`release-inputs.json` uses contract version 3. Its signed payload binds the
product, desktop version, channel, architecture, monotonic release sequence,
guest image ID, version, canonical staging name, guest path, exact byte sizes,
and lowercase SHA-256 digests of the other eight files. The signature is
Ed25519, and the signing worker receives only an external, channel-scoped
public-key trust set. No release
private key is stored in this repository or passed to the Windows signing job.

The packaging script reconstructs canonical signing bytes from the typed
manifest and verifies the signature before reading release inputs. It rejects
missing, additional, symlinked, oversized, wrong-architecture,
digest-mismatched, unsigned, or untrusted inputs. It also checks the VHDX file
signature and independent sidecar digest, and requires the signed guest record
to match the VHDX inventory record exactly.

`bin/components/grok-acp/catalog.json` is the bounded signed official-component
envelope consumed by the daemon. The packaging verifier independently checks
its canonical base64, strict duplicate-free JSON, Ed25519 signature and domain,
key ID, positive sequence, expiry, exact `grok-build`/`xAI` identity, Windows
target architecture, semantic version, `bin/grok.exe` path, size, and lowercase
SHA-256 digest. The selected executable must be the exact
`bin/components/grok-acp/bin/grok.exe` inventory record and an architecture-
correct PE. The daemon persists the runtime rollback watermark; stateless
packaging verification does not replace that policy.

Electron Packager copies the entire verified `bin` directory into `resources`.
The release command revalidates the resulting exact layout both before and
after binary signing:

```text
resources/bin/grok-daemon.exe
resources/bin/components/grok-acp/catalog.json
resources/bin/components/grok-acp/bin/grok.exe
```

The official Grok executable is vendor input and is never re-signed with the
Grok Desktop certificate. Its bytes are checked again after all first-party
Authenticode operations. The outer MSIX signature and inner ACP catalog bind
those preserved bytes. A stable approved xAI Authenticode signer policy should
be added when xAI publishes one; until then the signed catalog, external
provenance evidence, and byte preservation are mandatory.

The manifest shape is:

```json
{
  "version": 3,
  "product": "grok-desktop",
  "architecture": "x64",
  "channel": "stable",
  "desktopVersion": "1.0.0",
  "sequence": 1,
  "guest": {
    "imageId": "grok-guest-1.0.0",
    "imageVersion": "1.0.0",
    "stagingName": "grok-guest.vhdx",
    "path": "guest/grok-guest.vhdx",
    "sha256": "<64 lowercase hex characters>",
    "size": 123456
  },
  "files": [
    { "path": "<canonical path>", "sha256": "<64 lowercase hex characters>", "size": 123 }
  ],
  "signature": {
    "algorithm": "ed25519",
    "keyId": "<approved release key ID>",
    "value": "<base64 Ed25519 signature>"
  }
}
```

File records must be sorted by path. Trusted build tooling must call the
exported `releaseInputSigningBytes` function from
`apps/desktop/scripts/release-utils.mjs`; it signs the typed payload plus the
signature algorithm and key ID, while omitting only the signature value. This
keeps producer and verifier canonicalization identical.

`catalog/components.json` is specifically the official guest image catalog
consumed by the LocalSystem service. It is canonical JSON with this exact
schema:

```json
{
  "schemaVersion": 1,
  "product": "grok-desktop-guest",
  "architecture": "x64",
  "sequence": 1,
  "images": [
    {
      "id": "grok-guest-1.0.0",
      "version": "1.0.0",
      "stagingName": "grok-guest.vhdx",
      "sha256": "<64 lowercase hex characters>",
      "sizeBytes": 123456
    }
  ],
  "signature": {
    "algorithm": "ed25519",
    "keyId": "<approved release key ID>",
    "value": "<base64 Ed25519 signature>"
  }
}
```

The release contains exactly one catalog image. Its architecture and sequence
must match the outer manifest, and its ID, version, staging name, SHA-256, and
size must match the outer guest record and packaged VHDX exactly. Records are
sorted by ID. Trusted catalog signing tooling must reproduce
`guestImageCatalogSigningBytes` from
`apps/desktop/scripts/release-utils.mjs`; the Go service exposes the equivalent
`GuestImageCatalogSigningBytes` contract. Both cover every typed catalog field
except the signature value.

The guest and integration catalogs are byte-for-byte bound by the signed
release-input manifest. They contain no API key, OAuth token, signing secret, or
arbitrary provider endpoint. The guest catalog has its own signature and
service-owned rollback state; the outer release-input signature is not a
substitute for that runtime policy.

Before package assembly, the integration catalog is decoded as bounded,
duplicate-key-free UTF-8 JSON and checked against the guest runner's version 1
catalog shape. Bundle identities and locations, semantic versions, capability
sets, portable relative paths, file inventories, SHA-256 values, sizes,
executable flags, and manifest bindings are validated in canonical order. The
guest still repeats its stricter handle-based bundle and signed-manifest
verification at install and execution time.

### Trusted native builds

The release worker first builds `bin/grok-daemon.exe` with the public ACP trust
set compiled into the binary:

```powershell
pnpm build:windows-daemon -- --arch x64 --out release-inputs/windows/x64/bin/grok-daemon.exe
pnpm build:windows-daemon -- --arch arm64 --out release-inputs/windows/arm64/bin/grok-daemon.exe
```

The command requires absolute, regular-file `GROK_WINDOWS_CARGO_PATH`,
`GROK_WINDOWS_RUSTC_PATH`, and `GROK_WINDOWS_LINKER_PATH` values, plus a
pre-provisioned `GROK_WINDOWS_CARGO_CACHE` directory containing registry cache
data only. Only regular `registry/index` data and `registry/cache` crate
archives are copied into an ephemeral `CARGO_HOME`; pre-extracted
`registry/src` content is never trusted. Cargo configuration, credentials,
symlinks, and special files are rejected. The build is both `--locked` and
`--offline`, so the qualified worker must hydrate this cache before entering
the release boundary.

`GROK_WINDOWS_TOOLCHAIN_ENV_JSON` supplies the qualified MSVC environment as a
strict JSON object with exactly these fields:

```json
{
  "systemRoot": "C:\\Windows",
  "executablePaths": ["C:\\Rust\\bin", "C:\\BuildTools\\bin"],
  "includePaths": ["C:\\BuildTools\\include"],
  "libraryPaths": ["C:\\BuildTools\\lib"],
  "librarySearchPaths": ["C:\\BuildTools\\libpath"]
}
```

Every entry must be a unique absolute local path to a regular directory; UNC
paths and inherited search paths are rejected. Cargo runs from an ephemeral
working directory with isolated home, target, and temporary directories. Its
child environment is rebuilt from this toolchain contract and cannot inherit
worker `HOME`, `CARGO_HOME`, `PATH`, Rust flags, wrappers, or Cargo config.

The public-only, bounded `GROK_ACP_CATALOG_TRUSTED_KEYS` contract uses records
in canonical `key-id=64-lowercase-hex` form. Records are ordered by key ID and
separated by semicolons. The build uses `--locked`, `--offline`, `--release`, a fixed Windows MSVC target,
and `--no-default-features`; it cannot enable `debug-acp-descriptor` or inherit
ambient Rust flags/wrappers. It injects the exact trust contract and a SHA-256
`grok-acp-catalog-trust-v1` binding. The daemon recomputes that binding at
runtime, and packaging scans the architecture-correct PE for both values.

Release builds reject legacy `GROK_ACP_EXECUTABLE`, `GROK_ACP_VERSION`,
`GROK_ACP_SHA256`, and `GROK_ACP_WORKSPACE_ROOTS` overrides. Those variables do
not select a component outside the signed managed catalog.

The release worker builds `service/grok-vm-service.exe` from the reviewed source
before the outer input manifest is signed:

```powershell
pnpm build:windows-service -- --arch x64 --out release-inputs/windows/x64/service/grok-vm-service.exe
pnpm build:windows-service -- --arch arm64 --out release-inputs/windows/arm64/service/grok-vm-service.exe
```

The command requires an absolute `GROK_WINDOWS_GO_PATH` and the public-only
`GROK_RELEASE_METADATA_PUBLIC_KEYS_JSON`. It uses an allowlisted child
environment, `CGO_ENABLED=0`, `GOTOOLCHAIN=local`, `-mod=readonly`, `-trimpath`,
and `-buildvcs=false`. `GOENV=off` and `GOWORK=off` prevent worker-profile or
workspace flags from entering the build; module downloads use the public Go
proxy and checksum database without private-module exceptions. It converts
each approved Ed25519 SPKI key to its canonical raw 32-byte form, serializes the
key-ID map deterministically, base64-encodes it for a shell-independent Go
linker value, and injects both `main.guestCatalogTrust` and a SHA-256
`main.guestCatalogTrustBinding`. The service recomputes that binding on startup.

The packager scans the architecture-correct PE for the exact encoded trust map
and binding derived from its approved public-key set. A generic cross-build, a
binary built with a different key set, or a binary relying on runtime trust is
rejected even when it appears in an otherwise signed input inventory. Windows
has no environment or command-line override for catalog trust or release root.

### Deferred MSIX signing boundary

The release environment supplies:

```text
GROK_MSIX_IDENTITY
GROK_MSIX_PUBLISHER
GROK_MSIX_PUBLISHER_DISPLAY_NAME
GROK_WINDOWS_MAX_TESTED_VERSION
GROK_WINDOWS_SIGNTOOL_PATH
GROK_WINDOWS_POWERSHELL_PATH
GROK_WINDOWS_TIMESTAMP_SERVER
GROK_WINDOWS_SIGNER_SHA1
GROK_WINDOWS_SIGN_ARGS_JSON
GROK_WINDOWS_CARGO_PATH
GROK_WINDOWS_RUSTC_PATH
GROK_WINDOWS_LINKER_PATH
GROK_WINDOWS_CARGO_CACHE
GROK_WINDOWS_TOOLCHAIN_ENV_JSON
GROK_WINDOWS_GO_PATH
GROK_RELEASE_METADATA_PUBLIC_KEYS_JSON
GROK_UPDATE_TRUSTED_KEYS_JSON
GROK_ACP_CATALOG_TRUSTED_KEYS
GROK_XAI_COMPONENT_PROVENANCE_EVIDENCE_ID
GROK_XAI_COMPONENT_REDISTRIBUTION_EVIDENCE_ID
```

`GROK_WINDOWS_SIGN_ARGS_JSON` is a bounded argument array passed directly to
SignTool without a shell. It must select the certificate identified by
`GROK_WINDOWS_SIGNER_SHA1`. The only accepted options are `/sha1`, optional
store selectors `/s` and `/sm`, and the paired hardware-provider selectors
`/csp` and `/kc`. File certificates, passwords, automatic certificate
selection, caller-selected digest/timestamp options, and arbitrary SignTool
arguments are rejected. Ambient Electron signing variables such as
`WINDOWS_CERTIFICATE_FILE`, `CSC_LINK`, and their password variants also abort
the release. Signing and verification subprocesses receive a minimal allowlist
of non-secret operating-system environment variables. The timestamp endpoint
must use HTTPS, except for the exact Microsoft-documented DigiCert RFC 3161
endpoint `http://timestamp.digicert.com`. That narrow exception is necessary
because Windows SignTool rejects the HTTPS form on the qualified worker. RFC
3161 sends a digest rather than the signed artifact or private key, and the
returned timestamp token is cryptographically verified. Other unauthenticated
HTTP origins, ports, paths, credentials, queries, and fragments remain rejected.

`GROK_RELEASE_METADATA_PUBLIC_KEYS_JSON` maps approved release key IDs to
base64-encoded Ed25519 SubjectPublicKeyInfo documents. It contains public trust
anchors only. The same exact set is compiled into the VM service by the trusted
service build above and verifies both release-input and guest-catalog
signatures. Promotion policy supplies a channel-appropriate set and enforces
monotonic release sequence independently of this stateless packaging command.

`GROK_UPDATE_TRUSTED_KEYS_JSON` separately maps the stable-channel update key
IDs to canonical base64 Ed25519 SubjectPublicKeyInfo documents. The packager
embeds this public-only trust set as `resources/update-trusted-keys.json`; the
Electron main process requires it before any platform updater can run. Keep
release-input and update signing key scopes separate even though their encoded
formats are identical.

The two xAI component evidence IDs are bounded, non-secret references to the
release system records that prove the executable came from an approved xAI
distribution and that redistribution in Grok Desktop is permitted. Both are
required and are written to the release record. Catalog signatures establish
integrity and publisher identity; they do not grant copyright or redistribution
rights. The pipeline must fail when either evidence record or either required
component input is absent. This repository does not vendor an xAI executable,
private signing key, or production trust value.

The pipeline signs and verifies every first-party PE/DLL/native module before
MSIX assembly, then asks `electron-windows-msix` to assemble an unsigned
package. The pipeline
signs the final MSIX exactly once through the same explicit certificate-store or
hardware-backed SignTool path. After SignTool validation, PowerShell
`Get-AuthenticodeSignature` must report the exact expected certificate subject
and thumbprint plus a valid timestamp for every signed binary and the MSIX. The
release record contains that signer identity, the input signature key and
sequence, guest image version, input-manifest digest, final artifact digest,
size, package identity, channel,
architecture, and the inspected Electron fuse state.
The same record includes the ACP catalog sequence, expiry, signature key ID,
selected component version/path/digest, preserved-vendor-signature policy, and
the provenance and redistribution evidence IDs so runtime rollback or expiry
failures can be correlated without exposing key material.

### Deferred MSIX protocol activation

The rendered MSIX manifest registers the lowercase `grok-desktop` URI scheme
with the package's exact `app\Grok Desktop.exe` full-trust entry point. The
uap3 registration passes the exact activated URI as one quoted `%1` argument
to that executable and is emitted from
`release/windows/AppxManifest.xml.template` by the existing manifest renderer.
Release tooling does not create a second registry-based protocol handler.

The public activation surface is the versioned
`grok-desktop://open/v1/...` route grammar. The
`grok-desktop://app/index.html` origin remains private renderer content and is
not a public deep link. The desktop activation policy must reject that host,
unknown versions and routes, query strings, fragments, encoded paths, raw file
paths, prompts, commands, and arbitrary URLs before renderer navigation.

### Electron hardening

The packaged executable requires every known fuse to be specified. Release
assembly disables Run-as-Node, `NODE_OPTIONS`, Node inspection, and extra
`file://` privileges. It enables cookie encryption, embedded ASAR integrity,
ASAR-only application loading, and WebAssembly trap handlers. Source maps are
rejected from release output.

The renderer remains sandboxed and is served by the private
`grok-desktop://app` protocol. A packaged build cannot consume
`VITE_DEV_SERVER_URL`, and DevTools are disabled.

### Packaged service caveat

The MSIX declares a manual LocalSystem `desktop6:Service`. Microsoft requires
both the `packagedServices` and `localSystemServices` restricted capabilities
for this account. Installation therefore requires elevation, and Microsoft
Store acceptance is not assumed. Direct signed distribution and managed
enterprise deployment are the primary channels unless Microsoft explicitly
approves those capabilities.

The service must run under the Service Control Manager, derive every tenant from
an identification-only named-pipe token, and keep per-user state isolated beneath
service-owned roots. Production clients explicitly request
`SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`; stronger token levels are
rejected, and the service reverts before any resource access. A console-only or
startup-SID service binary is not a releasable input.

The daemon's read-only VM-service client additionally verifies that the
kernel-reported pipe-server PID is the running `GrokDesktopVmBroker` SCM process,
that it is configured as LocalSystem with the exact
`SERVICE_WIN32_OWN_PROCESS | SERVICE_PKG_SERVICE` type, and that both processes
have the exact package full name/family and fixed package-rooted paths. The
pinned Go runtime may report base own-process status, but shared, interactive,
user-service, and unknown flags remain rejected. The resulting capability
document is static broker readiness only. It does not enable Work or
`guest_control`; daemon proof-of-possession and the durable privileged operation
journal remain independent release gates.

### Deferred command

After the trusted inputs and production web/Electron build exist:

```powershell
pnpm package:windows -- --arch x64 --channel stable
pnpm package:windows -- --arch arm64 --channel stable
```

The signing environment and input artifacts are intentionally not synthesized
by this command. Real release workers must additionally run Windows App
Certification Kit, clean install/update/repair/uninstall tests, service recovery,
Virtual Machine Platform qualification, and the matrix in
`docs/quality/release-qualification.md` against the exact signed bytes.

## Platform references

- [Electron security checklist and custom protocol guidance](https://www.electronjs.org/docs/latest/tutorial/security)
- [Electron fuses](https://www.electronjs.org/docs/latest/tutorial/fuses)
- [electron-builder NSIS target](https://www.electron.build/nsis.html)
- [Microsoft Defender SmartScreen](https://learn.microsoft.com/en-us/windows/security/operating-system-security/virus-and-threat-protection/microsoft-defender-smartscreen/)
- [Electron MSIX updater behavior (deferred)](https://www.electronjs.org/docs/latest/api/auto-updater)
- [Microsoft packaged URI activation](https://learn.microsoft.com/en-us/windows/apps/develop/launch/handle-uri-activation)
- [Microsoft packaged service manifest](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-desktop6-service)
- [Microsoft MSIX deployment planning (deferred)](https://learn.microsoft.com/en-us/windows/msix/desktop/managing-your-msix-deployment-targetdevices)
