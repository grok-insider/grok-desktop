# Release qualification

Grok Desktop does not ship reduced-quality interim editions. A channel is
publishable only when the applicable gates below pass for the exact signed
artifacts. Passing unit tests is necessary but not sufficient.

## Artifact identity

- Reproducible Rust daemon, Go service, guest VHDX, managed integration bundles,
  and Electron application inputs are locked.
- Windows x64 and ARM64 MSIX packages retain the same package family identity
  and contain the matching native binaries.
- Executables, service binaries, MSIX packages, update metadata, guest images,
  and integration manifests are signed by channel-scoped keys. Test keys cannot
  be trusted by a release build.
- The official Grok Build executable retains its vendor bytes and is selected by
  the signed xAI catalog; Grok Desktop does not overwrite its Authenticode
  signature. Provenance and explicit xAI redistribution-permission evidence are
  approved for the exact digest. Cryptographic trust alone is not a license.
- SBOM, checksums, provenance, symbols, license inventory, and release notes are
  generated from the release build. Credentials and signing material never
  enter the repository or artifacts.
- Electron fuses are inspected after packaging. Run-as-Node, Node CLI options,
  Node inspection, embedded ASAR integrity bypass, and unnecessary file
  protocol privileges are disabled.
- The dependency audit is run after release-only tooling is installed. A
  packaging framework with an unresolved high-severity transitive advisory is
  not allowlisted merely because it runs in CI.

## Windows matrix

- Windows 11 current and previous supported servicing releases, x64 and ARM64.
- Packaged-service capability approval or the chosen direct/enterprise
  distribution exception is recorded; LocalSystem service installation and
  removal are tested under elevation and MDM deployment.
- Standard user, administrator, domain user, non-ASCII profile, long profile
  path, redirected profile, and OneDrive-known-folder configurations.
- Clean install, update across every supported schema/protocol boundary,
  repair, uninstall, downgrade rejection, channel switch, and enterprise
  MDM-owned update mode.
- Virtual Machine Platform ready, disabled, policy-blocked, firmware-disabled,
  nested virtualization unavailable, low disk, low memory, and service stopped.
- Enterprise proxy, TLS inspection with installed enterprise CA, offline start,
  captive portal, DNS failure, provider rate limit, and clock skew.
- Multi-monitor, mixed DPI, HDR, high contrast, reduced motion, screen reader,
  keyboard-only operation, IME, RTL content, and 200% text scaling.

## Security and recovery

- Renderer navigation, custom protocol, IPC sender, CSP, permissions, window
  creation, deep links, and Electron fuses pass adversarial tests.
- The installed MSIX registers exactly one lowercase `grok-desktop` protocol
  handler bound to `app\Grok Desktop.exe` with one quoted `%1` URI argument;
  cold and running-instance activation
  accept only the documented `grok-desktop://open/v1/...` grammar, while the
  private `grok-desktop://app` renderer origin and malformed inputs are ignored.
- Vault data, database pages/WAL/backups, diagnostics, crash reports, clipboard,
  notifications, telemetry, and support bundles pass secret canary tests.
- Database migrations are tested from every supported release. Interrupted
  migration, full disk, corrupt WAL, wrong vault key, backup restore, and newer
  schema all fail without silent data loss.
- Daemon, renderer, official ACP component, integration, VM service, guest, and
  host are each terminated at every durable transition. Recovery produces one
  result, a safe retry, or `interrupted_needs_review`; never an implicit repeat
  of a non-idempotent action.
- The exact signed x64 and ARM64 daemons exercise the real Tokio named-pipe
  client and explicitly request `SECURITY_IDENTIFICATION` with the SQOS-present
  flag. The service observes an identification token and rejects anonymous,
  impersonation, and delegation levels. The unreleased
  `SecurityImpersonation` behavior is not accepted as a compatibility mode.
- Pipe-server qualification proves that the kernel-reported server PID is the
  running `GrokDesktopVmBroker` SCM process, configured as a LocalSystem
  `SERVICE_WIN32_OWN_PROCESS | SERVICE_PKG_SERVICE`, with the broker and daemon
  at their exact paths rooted in the package and sharing the exact package full
  name and family. The pinned Go service runtime may report the live status type
  as base `SERVICE_WIN32_OWN_PROCESS`; no shared, interactive, user-service, or
  unknown flag is accepted. PID reuse, service stop/restart, update, and
  package/path mismatch races fail closed.
- A malicious pipe-first-instance fixture wins the fixed name before the real
  service on both architectures. It cannot obtain an impersonation-capable user
  token; the daemon rejects its non-SCM PID before sending even a non-secret
  capability request, then recovers only after connecting to the qualified
  service. Same-user denial of service remains possible and bounded.
- A successful read-only capability probe is recorded only as static broker
  readiness. It cannot advertise Work or `guest_control`; release tests keep
  both disabled until daemon proof-of-possession and the durable privileged
  operation journal pass recovery qualification.
- Reparse/junction/hard-link races, VHDX replacement, manifest traversal,
  signature rotation, rollback, oversized frames, slow peers, restart storms,
  and stale computer-use observations are exercised on real Windows workers.
- Guest tests prove no general NIC, SSH, console login, writable host share, or
  undeclared host socket is reachable.

## Product behavior

- Official Grok Build browser/device authentication, refresh, logout, revoked
  sessions, wrong organization, and component update/recovery are tested without
  importing cookies or user CLI custom-provider configuration.
- xAI BYOK setup, validation, rotation, deletion, scope/rate errors, and cost
  reporting use only `api.x.ai`; keys never appear in renderer snapshots or
  responses.
- The daemon-hosted Win32 BYOK prompt is exercised from the exact signed MSIX
  on x64 and ARM64. Tests cover package/window identity, arbitrary and stale
  HWNDs, destruction/reuse races, keyboard and screen-reader access,
  cancellation, daemon shutdown, memory-lock failure, and plaintext residue;
  renderer/preload/Electron-main telemetry must contain no entered bytes.
- SuperGrok plan identity and xAI API billing remain separate. Capability
  routing is driven by live ACP/model facts rather than a hard-coded plan-name
  matrix, and the applicable provider retention/ZDR state is surfaced without
  making an unsupported privacy claim.
- Projects, branches, messages, citations, attachments, artifacts, search,
  schedules, approvals, run history, export, and restore survive daemon and
  desktop restarts with ordering and optimistic revisions intact.
- Image, video, and voice surfaces handle cancellation, reconnect, partial
  output, provenance, content limits, device changes, and provider retirement.
- Wisp install, update, health failure, rollback, permission change, and removal
  are independently recoverable. Removing Wisp does not impair Chat, projects,
  or direct xAI features.
- Every visible command is functional or clearly unavailable with a stable
  reason. Loading, empty, offline, degraded, conflict, error, cancellation, and
  recovery states are keyboard and screen-reader accessible.

## Performance and soak

- Cold start, warm start, first token, stream rendering, search, project load,
  large thread, large library, VM boot, and update operations meet recorded x64
  and ARM64 budgets.
- Renderer and daemon memory remain bounded for a 24-hour mixed workload.
  Connections, subscriptions, file handles, child processes, VMs, and temporary
  artifacts return to baseline after cancellation and repeated window cycles.
- A seven-day automation/daemon soak covers sleep, resume, daylight-saving
  transitions, clock correction, missed-run policy, overlap policy, reboot, and
  network interruption.

## Release evidence

The release record contains artifact hashes, workflow and source revisions,
test matrix results, Windows App Certification Kit output, security scan and
dependency audit results, migration fixtures, performance comparison, known
limitations, and the person who approved promotion. Preview evidence cannot be
reused for stable after any byte changes.
It also records the ACP catalog sequence, expiry and signing key, selected Grok
Build version and digest, preserved-vendor-signature policy, source provenance,
and redistribution-permission evidence identifiers.
