# Platform execution threat model

- Status: foundation baseline
- Last reviewed: 2026-07-10
- Scope: Windows VM service, NixOS utility guest, managed integrations, and the
  computer-use channel

## Security objective

Strong Work may process attacker-controlled files, web content, model output,
tool metadata, and integrations. None of those inputs may acquire ambient host
authority. When the qualified isolation stack is missing or unhealthy, Grok
Desktop enters Limited Mode; it does not fall back to host execution.

This model complements the application authorization model. Approval answers
whether an action should run. The platform boundary limits what a compromised
or malicious approved workload can reach.

## Assets

- Host credentials, browser sessions, user files, input devices, clipboard,
  desktop pixels, and network identity.
- The daemon database, durable approval records, vault handles, update state,
  integration trust roots, and audit evidence.
- Guest image integrity, guest control channels, integration state, and
  reviewed output diffs.
- Availability of the host, VM service, guest, daemon, and user workspace.

## Trust boundaries

1. The sandboxed renderer and all displayed/model-supplied content are
   untrusted.
2. The per-user daemon is trusted to enforce grants but is not privileged to
   configure Windows virtualization.
3. The native Windows service is privileged. Its named-pipe transport must use
   an ACL for the owning SID, derive the peer SID from the connection token,
   and compare it with every request identity. Clients grant exactly
   `SecurityIdentification`; the service only queries that token and reverts
   before dispatch or resource access.
4. The NixOS guest kernel is an isolation boundary. It has no general guest NIC
   and communicates through purpose-specific host sockets. Guest control uses a
   per-boot authenticated v2 channel provisioned only by LocalSystem; a host CID
   is not caller authentication.
5. Every managed integration is a separate, untrusted process with an
   independently supervised lifecycle and the intersection of manifest, user,
   and guest-policy permissions.
6. Host workspace content crosses into the guest read-only. Guest writes stay
   in an overlay until the daemon presents and commits a reviewed diff.

Local administrators, Windows kernel compromise, hypervisor compromise, and
physical attacks are outside this boundary. They remain part of enterprise and
OS hardening rather than claims made by Grok Desktop.

## Required invariants

- The service exposes only `GetCapabilities`, `EnsureImage`, `CreateVm`,
  `StartVm`, `StopVm`, `DeleteVm`, `AttachWorkspace`, and the authorization-
  gated `GuestControl` method union. The legacy `OpenSocket` wire operation
  fails unavailable in production and never returns a raw endpoint. There is no
  generic HCS, filesystem, registry, shell, or PowerShell method.
- Image and workspace paths are relative to different service-owned roots.
  The service rejects traversal, UNC/drive paths, alternate data streams, and
  device names, then resolves reparse points through open handles at use time.
- `EnsureImage` verifies size and SHA-256 before an image becomes selectable.
  Production metadata and images are signed, rollback-protected release
  artifacts.
- A workspace attachment is always read-only. A running VM cannot change its
  attachments.
- Socket purposes are a closed allowlist. HCS service GUIDs are fixed,
  LocalSystem-only, and disallow wildcard binds; unlisted services have deny-all
  descriptors. The service, not the daemon, owns native HVSock connections.
- Guest channel v2 uses fresh 128-bit boot IDs, 256-bit keys, and independent
  256-bit host and guest nonces. Deterministic protobuf payloads are
  authenticated with domain-separated HMAC-SHA-256 before control JSON is
  decoded. Direction, boot, exact sequence, request ID, deadline, and size
  mismatches fail closed.
- Release integrations have a valid Ed25519 signature from an accepted key.
  Unsigned manifests are valid only as source on the development channel and
  are rejected by the guest runner.
- Adapter processes are invoked by direct executable path and argv, never
  through a shell. Message size, initialization time, in-flight requests,
  restart count, and output are bounded.
- A computer-use action names the exact application instance and observation
  revision. The adapter rejects stale observations or changed applications
  before injecting input. The v1 action union contains no arbitrary execution.
- Secrets, typed text, frames, and workspace contents are excluded from normal
  logs and crash metadata.
- Production named-pipe clients explicitly request identification-only SQOS.
  The broker rejects anonymous, impersonation, and delegation tokens, and no
  resource access occurs while the identification token is active.

## Threats and controls

| Threat | Primary controls | Residual risk / qualification |
| --- | --- | --- |
| Cross-user calls to the privileged service | Named-pipe ACL, identification-only peer token, request SID equality, per-user resources | Same-SID malware can cause bounded lifecycle denial of service; rate limits and daemon proof-of-possession remain qualification work. |
| Fixed-name pipe squatter captures a desktop connection | Client requests `SecurityIdentification` and verifies the kernel-reported server PID against the running LocalSystem SCM service, exact package identity, and fixed paths before its first write | A squatter can deny service, but cannot obtain an impersonation-capable token or receive even the capability probe. Never send secrets over this transport. |
| Same-SID process connects directly to guest control | LocalSystem-only HCS descriptors, per-boot provisioning proof, authenticated direction/sequence/deadline, service-owned HVSock proxy, same-package path/PID continuity, no raw socket return | Daemon proof-of-possession, durable privileged replay, and real Windows qualification remain incomplete; the proxy grant stays disabled and production remains Limited Mode. |
| Path traversal, Windows device path, ADS, or reparse race | Fixed roots, portable lexical rejection, handle-based final-path validation, image digest | Windows tests must cover junctions, symlinks, mount points, hard links, case folding, and replacement races. |
| Malicious or rolled-back guest image | Independently signed guest catalog, release-bound digest and size, fixed service-owned staging, and a durable monotonic sequence watermark | Real Windows replacement, interrupted update, downgrade, equivocation, and trust-key rotation remain release qualification gates. |
| Guest escape through broad virtualization API | Closed service contract, bounded CPU/memory, no caller-selected HCS or guest transport payload | HCS configuration and Windows builds require independent review and qualification. |
| Host file modification from the guest | Read-only mount, guest overlay, reviewed diff commit by daemon | Parsers and diff presentation can still be attacked by hostile names/content. |
| Data exfiltration from an integration | No general NIC, explicit socket purposes, manifest grants, no secrets by default | Any data deliberately sent to a broker must be shown in the approval disclosure. |
| Integration supply-chain compromise | Strict schema, canonical signed manifest, publisher key policy, isolated lifecycle | Signing does not establish publisher intent; provenance and reproducible bundle builds remain required. |
| Prompt-injected computer-use action targets a new window | Observation revision plus application and instance IDs, bounded closed action union | Pixel-perfect overlays inside the same application remain possible and require approval/context UX. |
| Adapter protocol memory/CPU denial of service | 8 MiB message ceiling, one in-flight Wisp request, timeouts, health threshold, restart limits | Guest resource quotas and restart backoff must be enforced by the runner implementation. |
| Sensitive diagnostics | Dedicated protocol stdout, diagnostic-only stderr, redaction, size/retention limits | Native crash dumps and Windows event logs require explicit redaction tests. |

## Failure behavior

Any identity mismatch, invalid path, unverified image, unsupported protocol,
signature failure, writable mount request, stale observation, wrong application,
unknown socket purpose, or unavailable backend fails closed. The daemon records
the reason and exposes Limited Mode. It does not retry a non-idempotent action or
route it to the host.

An invalid guest-channel MAC, stale boot ID, wrong direction, sequence gap,
conflicting replay, metadata mismatch, expired deadline, or replay-capacity
overflow poisons and closes the channel. The guest does not decode or dispatch
the embedded control JSON until frame authentication succeeds.

The current non-Windows VM service is a stateful contract simulator and reports
`simulated: true`. It is test infrastructure, never an isolation boundary. The
Windows constructor uses the authenticated named-pipe transport and direct,
fixed-surface Compute Core calls. It returns unavailable when HCS or
VirtualMachinePlatform cannot be probed and never substitutes the simulator.
Cross-compilation and fake-backed tests are not evidence of Windows isolation;
the qualification gates below still apply before release.

The Rust `grok-vm-service-client` exposes only a bounded `get_capabilities`
probe and returns unavailable on non-Windows platforms. A successful response
proves static broker compatibility after client-side server qualification. It
does not prove a live guest, authorize a lifecycle or guest-control operation,
or satisfy daemon proof and durable replay requirements. Work remains in
Limited Mode when any of those independent facts is absent.

## Qualification gates

- Windows x64 and ARM64 service tests using the real Tokio client, real
  identification-only named-pipe tokens, and malicious pipe-first-instance and
  filesystem fixtures. The matrix must prove that a squatter cannot obtain an
  impersonation-capable user token or receive a request, and that the daemon
  rejects a server PID, account, configured packaged-service type, reported
  own-process type, path, or package identity that does not match the running
  SCM service.
- HCS guest lifecycle, crash recovery, suspend/resume, host reboot, and orphan
  cleanup tests.
- Real HVSock provisioning and proxy tests covering LocalSystem ACLs, rejection
  of tenant/admin direct connections, service and VM restart key loss, package
  identity continuity, handshake timeout, replay, and in-process key zeroing.
- VHDX signature, digest, interrupted update, rollback, and low-disk tests.
- Read-only sharing and overlay export tests with reparse points and hostile
  filenames.
- Guest egress tests proving that no general NIC or undeclared socket is usable.
- Manifest canonicalization, key rotation/revocation, protocol compatibility,
  oversized message, restart storm, and secret-redaction tests.
- Computer-use race tests that switch focus, replace windows, replay actions,
  and mutate the surface between observation and injection.

## Caller continuity work

Named-pipe authentication now proves the Windows user, logon session, client
PID and creation time. It also recognizes the exact daemon executable only when
the process has the broker's own MSIX package full name and family. That fact is
necessary but does not grant guest control. The current ten-minute idempotency
cache is bounded but memory-only; HCS lifecycle intent survives a restart,
while replay of the exact transport result does not. Production qualification
therefore still requires:

- A per-install daemon proof-of-possession key protected outside renderer and
  integration processes, with challenge-bound requests covering the operation,
  payload digest, idempotency key, and deadline.
- Rotation and revocation that cannot silently fall back to SID-only authority.
- A bounded durable replay journal keyed by caller identity and idempotency key,
  storing the request digest and terminal result before acknowledgement.
- Recovery rules that return an interrupted result for an uncertain side effect;
  they must never automatically replay a non-idempotent operation.
