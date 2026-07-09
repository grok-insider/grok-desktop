# Windows VM service contract

This Go module defines the narrow privileged boundary used to manage the Grok
Desktop NixOS guest. The lifecycle operation set is:

- `GetCapabilities`
- `EnsureImage`
- `CreateVm`
- `StartVm`
- `StopVm`
- `DeleteVm`
- `AttachWorkspace` (read-only only)
- `GuestControl` (qualified authenticated proxy only)

`OpenSocket` remains in the v1 wire decoder for compatibility, but the Windows
backend returns `unavailable` and never returns a raw HVSock endpoint.
`GuestControl` is a contract 1.1 operation with a closed runner-method union. It
is omitted from production capabilities and rejected before service dispatch
until the caller has both packaged-process qualification and daemon
proof-of-possession.

There is intentionally no arbitrary command, PowerShell, registry, filesystem,
or generic HCS method. Wire payloads contain no identity field. The host derives
the current user's SID from the transport, then injects it into the internal
request immediately before dispatch.

Paths are relative to service-owned image and workspace roots. The shared
validator rejects traversal, drive/UNC paths, alternate data streams, and Windows
device names on every OS. Windows resolves reparse points through open handles,
checks volume/file identity, and revalidates each disk and workspace immediately
after HCS consumes the fixed configuration. These handles share read/write as
required by HCS, but never delete access, so the validated object cannot be
renamed or replaced during configuration. Image inputs must use the exact
`staging/<stagingName>` path declared by the active signed guest catalog.

## Official guest image policy

The LocalSystem backend accepts only official images listed in the Ed25519
signed `resources/catalog/components.json` shipped beside the service. On
Windows, the service derives `resources` from its own fixed
`resources/service/grok-vm-service.exe` location; no request, flag, environment
variable, or renderer value can select another release root. The service binary
must contain a base64-encoded, public-only key-ID trust map in the
`main.guestCatalogTrust` linker variable and its SHA-256 binding in
`main.guestCatalogTrustBinding`. A production binary without both matching
values fails at startup.

The strict catalog schema is:

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
    "keyId": "<approved guest release key ID>",
    "value": "<base64 Ed25519 signature>"
  }
}
```

Image records are sorted by `id`. `GuestImageCatalogSigningBytes` is the
producer/verifier canonicalization contract: it covers every typed field,
including signature algorithm and key ID, and omits only the signature value.
The decoded linker document maps key IDs to canonical base64 encodings of raw
32-byte Ed25519 public keys. Release tooling derives it deterministically from
the approved SPKI public trust set. Private keys never enter the package or
service process.

Before opening its listener or creating a tenant, the service verifies the
catalog signature and architecture, secures the shared data root, and records
the highest accepted sequence in `guest-image-policy.json`. A lower sequence or
different catalog bytes at the same sequence fail closed. Updates are atomic
and cross-process locked. Windows rejects reparse points in catalog and policy
paths.

On Windows, the storage root is fixed to
`ProgramData\Grok Desktop\VM Service`; `--data-root` is available only to the
non-Windows development simulator. The service creates or adopts the
`Grok Desktop` and `VM Service` directories, tenant directories, and fixed
`.vm-service` metadata directories through handle-relative Windows opens. New
directories receive their owner and protected DACL atomically, while existing
directories are validated and repaired through their handles. SYSTEM remains
the owner and group, tenant access is scoped to the configured SID, every
component must be a direct non-reparse directory, and non-delete-sharing
handles plus final path and file-identity checks pin the hierarchy throughout
setup.

`EnsureImage` retains the v1 `sha256` and `sizeBytes` request fields for wire
compatibility, but they are assertions rather than authority. If present they
must equal the signed values; if omitted the signed values are still used for
copy verification and persisted metadata. Unsigned image IDs and noncanonical
staging names are rejected. Stored image metadata is checked again before VM
disk copy and HCS boot. Release engineering should use versioned immutable image
IDs so catalog evolution does not reinterpret an installed ID.

`NewPlatformService` returns a stateful simulator on non-Windows systems. Its
capability response sets `simulated: true`; it validates authorization, paths,
resource limits, read-only mounts, socket purposes, and VM lifecycle transitions.
On Windows, the backend calls the documented `computecore.dll` HCS API directly.
It probes VirtualMachinePlatform before accepting requests and fails closed if
HCS, schema 2.1 VM creation, read-only Plan9, or the allowlisted Hyper-V socket
configuration cannot be enforced. It never falls back to the simulator.

Installed images and VM definitions use atomic, service-owned metadata beneath
`ImageRoot/.vm-service`. A restart adopts known running HCS systems, converts a
missing runtime to `stopped`, finishes interrupted deletion, and terminates an
owned HCS system that has no corresponding metadata. HCS receives a fixed Linux
utility VM document with no host network adapter and no guest process surface.

## Authenticated guest channel v2

Generated protobuf types and the shared codec live in `guestchannel/v2` and are
generated from `proto/guest/v2/channel.proto`. Each host session consumes a
fresh CSPRNG-generated 128-bit boot ID, 256-bit channel key, and 256-bit host
nonce. The guest contributes a 256-bit nonce and proves possession using the
domain-separated deterministic protobuf contract in ADR 0002.

Subsequent length-prefixed protobuf frames authenticate boot ID, direction,
sequence, request ID, deadline, and bounded control bytes with HMAC-SHA-256.
Sequences start at one in each direction. Only byte-identical authenticated
replays can return a cached response; gaps, conflicts, stale boots, invalid
MACs, and expired deadlines poison the channel. Closing a session zeroes its
key and cached control buffers.

HCS bind and connect descriptors for fixed service GUIDs admit LocalSystem
only. The Windows connector uses HCS `RuntimeId`, the Hyper-V VM GUID, and the
fixed AF_VSOCK port-derived service GUID. A successful VM start now includes
the authenticated control handshake. Channel ownership is keyed by tenant VM,
runtime GUID, and purpose; stop, delete, broker shutdown, cancellation, invalid
MAC/protocol data, and semantic response corruption retire the connection and
zero its key. Broker restart rekeys every adopted running VM and records it as
stopped after terminating HCS if rekeying fails.

The named-pipe `guest_control` operation accepts only the closed guest-runner
method set and JSON object parameters. The service creates the authenticated
operation ID and deadline, validates the correlated guest response, and never
returns a socket or caller-selected transport frame. The operation remains
authorization-gated in production because daemon proof-of-possession and the
typed journal-bound dispatch gateway are not implemented. The daemon-side
durable privileged-operation journal and startup recovery exist, but are not
wired to this service and grant no guest-control authority. There is no v1
downgrade.

## Runnable host

`cmd/grok-vm-service` serves versioned UTF-8 JSON Lines envelopes. It enforces an
8 MiB default frame limit, a 30 second maximum deadline horizon, 16 concurrent
connections, strict operation payloads, and a bounded ten-minute idempotency
cache. Every operation except capability discovery requires an idempotency key.
Shutdown stops
accepting new clients, interrupts idle reads, lets active requests finish, then
force-closes them after the configured grace period.

The response cache is intentionally described as memory-only, not durable
exactly-once execution. VM lifecycle intent is durable, but production caller
continuity still requires daemon proof-of-possession and a bounded persistent
request-digest/result journal as specified in the platform threat model.

Windows uses a fixed named pipe. Its DACL permits SYSTEM, Administrators, and
authenticated local users, but authorization does not rely on the DACL alone.
After each bounded frame read, the server obtains an identification-only client
token, reads its SID and logon proof, immediately reverts, and derives an
isolated tenant from that SID. Production clients must connect with
`SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`; impersonation and delegation
tokens are rejected. No service code may access a filesystem, registry,
network, or other resource while the client token is active. This prevents a
spoofed fixed-name pipe from acquiring authority to act as the desktop user.
The service also binds the connection to the client PID and creation time.
Windows marks packaged-daemon qualification only when the kernel-reported image
is the exact `resources\bin\grok-daemon.exe` and its package full name and
family equal the broker's own package. Any identity or process change closes
the connection. This qualification is necessary but does not set the guest
grant without the separate proof-of-possession protocol.

Non-Windows builds use a loopback-only development listener and expose an
in-memory authenticated listener for integration tests. Both are simulators,
not production identity boundaries.

Example development start:

```sh
export GROK_VM_RELEASE_ROOT=/tmp/grok-release/resources
export GROK_GUEST_CATALOG_TRUST='{"release-2026":"<base64 raw Ed25519 public key>"}'
go run ./cmd/grok-vm-service \
  --data-root /tmp/grok-vm \
  --development-user-sid S-1-5-21-1000-1001-1002-1003
```

The development release root must contain a correctly signed
`catalog/components.json`; test-only unsigned catalogs are not accepted. The
non-Windows trust environment escape hatch is not compiled into the Windows
path.

The request envelope is:

```json
{"version":"1.0.0","id":"request-1","operation":"get_capabilities","deadline":"2026-07-10T12:00:00Z","payload":{}}
```

The timestamp is illustrative; clients set it to a current UTC deadline no more
than 30 seconds ahead. The host returns one structured success or error envelope
with the same ID.

## Manifest verification

`manifestverify` strictly decodes managed-integration manifests, validates
every required typed field and bound from the manifest schema, rejects duplicate
JSON keys and null-as-zero ambiguity, validates bundle and guest paths,
permission collections, lifecycle policy, and protocol compatibility, then
applies publisher-trust and capability policy before signature verification.
Errors identify the failed rule without echoing manifest values.

Stable, preview, and nightly manifests cannot be unsigned. Development
manifests are accepted unsigned only when the policy enables development mode
and includes the publisher in `UnsignedDevelopmentPublishers`. The publisher's
declared trust must match `PublisherTrust`. Release signing must use
`manifestverify.SigningBytes`; it canonicalizes the typed manifest with an empty
signature value before Ed25519 signing.

Run the Go gates with:

```sh
go test ./...
go test -race ./...
go vet ./...
GOOS=windows GOARCH=amd64 go build ./cmd/grok-vm-service
GOOS=windows GOARCH=arm64 go build ./cmd/grok-vm-service
```

The Windows-only tests compile the Compute Core handle/HRESULT ABI and fixed
schema 2.1 configuration for both architectures. Actual lifecycle, reparse,
Plan9, Hyper-V socket, impersonation, and host-reboot qualification must run on
Windows workers with VirtualMachinePlatform enabled.
