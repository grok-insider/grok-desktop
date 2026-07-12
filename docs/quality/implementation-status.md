# Implementation status

- Snapshot date: 2026-07-12
- Release status: not qualified for distribution
- Wire/schema tip: **IPC protocol epoch 16**, **SQLCipher schema 19** (scheduler
  journal kernel; execution still disabled). Older narrative below that still
  says “current IPC v15 / schema 18” describes retained contracts under those
  later epochs unless an epoch explicitly replaced them.
- Linux full product GA contract and milestones:
  [linux-ga.md](linux-ga.md). Platform isolation on Linux is **specified**
  (platform ADRs 0004–0007) and **not implemented**; Work remains Limited Mode
  on Linux until the QEMU/KVM broker, virtio guest image, PoP, and privileged
  gateway qualify.

This ledger distinguishes implemented code from an end-to-end product workflow
and from release evidence. A locally passing adapter or cross-compiled binary
does not make its UI capability available.

IPC epochs, SQLCipher schemas, and durable contract narrative:
[protocol-and-persistence.md](../architecture/protocol-and-persistence.md).
Architecture principles:
[principles.md](../architecture/principles.md). Doc map:
[docs/README.md](../README.md).

## Implemented foundations

- Sandboxed Electron/React presentation shell with narrow preload validation,
  private application protocol, navigation and permission denial, daemon
  supervision, and hardened release-fuse checks.
- Rust domain and application layers with durable run/approval state machines,
  idempotent workspace mutations, encrypted SQLCipher persistence, OS-vault
  adapters, bounded Protobuf IPC, and daemon-owned capability resolution.
- Durable direct-BYOK conversation turns with immutable request fingerprints,
  provider-start intent, terminal failure/cancellation/uncertainty states,
  canonical user and assistant messages, citations, usage, per-response ZDR
  observation, and restart-stable history pagination. Every SQLCipher load now
  passes through validated `ConversationTurn` rehydration, which rejects
  lifecycle, dispatch-evidence, outcome, revision, and timestamp combinations
  that the aggregate could not have produced.
- Before IPC starts, the daemon recovers at most 100 incomplete conversation
  turns without provider I/O: undispatched reservations become `cancelled`, and
  provider-started turns become `interrupted_needs_review`. Exactly 100 can be
  recovered and served; with 101, the bounded first pass is committed, startup
  fails before IPC, and the next launch safely continues the remaining entry.
- Provider-derived terminal data is validated before commit. Malformed text,
  failures, continuation IDs, citations, or usage, and a rejected atomic
  terminal-store commit, are routed through one optimistic
  `interrupted_needs_review` fallback rather than being accepted as a known
  result. Current Responses parsing requires an exact embedded completed
  object/status, bounded response ID, and integer input/output/cost accounting;
  it never defaults missing terminal usage to zero. Official incomplete token
  and time-limit reasons become sanitized known failures with explicit
  retryability, while unknown reasons remain a local protocol failure. Citation
  extraction follows only exact official annotation paths in provider order,
  and reasoning deltas never enter assistant text. Success and known failure are
  buffered until `[DONE]` or clean EOF; any post-terminal data, duplicate or
  opposite terminal, transport failure, or decoder residue becomes
  `interrupted_needs_review`. Recognized malformed citations and the 257th
  unique citation fail closed, duplicate citations at the bound remain safe,
  and image responses require explicit cost ticks. Terminal transitions use the
  maximum of wall time and durable
  turn/run/effect timestamps. Citation URLs require credential-free HTTPS with
  a parsed host; aggregate citation and JavaScript-safe usage bounds guarantee
  exact representation through SQLCipher, Protobuf, and Electron. History
  materializes one result plus one look-ahead row before full-envelope encoding,
  then applies a resumable cursor at the exact 4 MiB IPC budget; one maximum
  canonical turn is guaranteed to fit. Provider context is bounded before row
  sets are cloned. Prior prompts
  from incomplete, cancelled, failed, or review-required turns are excluded
  from later provider context. Focused evidence covers the exhaustive domain
  state matrix and exact bounds, corrupt SQL command/history reads, malformed
  post-dispatch output, ambiguous/rejected terminal-store acknowledgements,
  clock rollback, and real-daemon 100/101 startup continuation.
- Renderer-free credential enrollment through daemon-hosted masked Win32 UI on
  Windows and a bounded Assuan pinentry exchange on Linux. The shared path has
  serialized prompt policy, durable intent replay, daemon-owned
  validation/vault persistence, and non-secret responses introduced in IPC v2;
  current IPC v15 retains that boundary. Linux additionally uses a protected
  canonical pinentry executable, a cleared and validated child environment,
  process-group cancellation/reaping, a one-shot stdin nonce handoff, and a
  pre-runtime non-dumpable daemon. The Windows
  path additionally qualifies the packaged owner window and locks and zeroizes
  entry buffers.
- Official-only xAI adapters for model discovery, Responses streaming, hosted
  search tools, and image generation at the fixed `https://api.x.ai` origin.
- Signed official Grok Build component catalog verification, isolated
  application-owned configuration, bounded ACP transport, permission mapping,
  and host-control health/auth negotiation.
- Signed guest-image catalog, hardened Windows service storage, explicit HCS
  lifecycle contract, authenticated guest channel v2, reproducible NixOS guest,
  and fail-closed capability reporting.
- IPC v4 bounded resumable long polling over durable per-run audit events,
  including strict cursor validation, dedicated reconnecting Electron
  connections, and process/subscriber concurrency limits. This is not provider
  token streaming; `Envelope.Event` remains unused.
- IPC v5 live official xAI Chat model discovery and daemon-owned schema-9
  selection, with canonical alias handling, no silent fallback, and
  selected-model readiness included in capability resolution.
- IPC v6 daemon-owned workspace search with canonical conversation routing,
  strict Electron result validation, SQLCipher schema-10 reconstruction of the
  derived search cache from canonical project, thread, message, artifact, and
  automation rows, and removal of generic renderer-accessible run transition
  and approval-request producer mutations.
- IPC v7 durable asynchronous direct Chat. Start returns a schema-12 reservation
  before provider completion; a bounded daemon task owns dispatch; normalized
  UTF-8 text events replay through dedicated acknowledged long polls; and exact
  cancellation commits `cancelled` before dispatch or
  `interrupted_needs_review` after dispatch before signalling the task. Durable
  cancellation commands replay across restart and bind an exact terminal race
  winner without overwriting it. Electron retains acknowledged cursors across
  daemon restart, validates replay-zero projections independently, and reloads
  the canonical terminal aggregate before acknowledging terminal delivery.
- IPC v8 safe direct-Chat Retry. The request carries only source identity and
  exact revision; the daemon freezes prompt, context, model, and a daemon-local
  credential enrollment generation. SQLCipher schema 13 stores immutable
  original/retry lineage, one-time thread generation binding, one retry child
  per source, and depth bounded to 64. Only the latest cancelled or explicitly
  retryable known-failed attempt is eligible. Completed, uncertain,
  legacy-unbound, generation-mismatched, superseded, depth-exhausted, and
  archived sources fail closed before provider dispatch. Lineage seals frozen
  context rows, while a coherent credential-use gate serializes key/binding
  reads and provider initiation against replacement/deletion. The binding is
  not official xAI identity, is not key-derived, and is not exposed through
  renderer IPC. See
  [ADR 0015](../decisions/0015-safe-direct-chat-retry-lineage.md).
- IPC v9 daemon-owned child-thread Branch, Edit-and-branch, and Regenerate.
  Branch is provider-free; Edit/Regenerate are explicit new billable attempts
  using the exact daemon-loaded source context, recorded model, and current
  matching local credential generation. SQLCipher schema 14 atomically stores
  immutable thread/message derivations, inherited completed-assistant outcomes,
  bounded family metadata, optional child turn/run/events, and exact replay.
  Memory and SQLCipher coverage includes parent immutability, nested families,
  family/depth bounds, restart, rollback, corruption, generic-store bypass,
  metadata count/byte limits, and conflicting command reuse. Strict Electron
  validation rejects malformed exact raw ancestry, omitted/reordered hidden or
  visible context, copied outcomes/citations, duplicate or concurrently active
  turns, model, actionable inherited context, and immutable identity projections;
  the renderer provides family switching and accessible billable confirmations.
  Inherited-action unavailability is visible as text as well as disabled-button
  naming. No official account identity is inferred from the local generation binding.
  Same-process ambiguous fork keys replay exactly.
  See [ADR 0016](../decisions/0016-daemon-owned-conversation-forks.md).
- IPC v10 durable conversation-fork result delivery. SQLCipher schema 15 creates
  a Pending/revision-0 delivery atomically with every fork, resolves exact keys
  before bounded pending-fingerprint aliases, and moves to
  Acknowledged/revision-1 only through an exact idempotent command. Alias growth
  is capped at 64 per child; canonical/alias key collisions, malformed delivery
  pairs, duplicate pending fingerprints, partial acknowledgement writes, and
  corrupt correlations fail closed. Schema-14 rows migrate to acknowledged
  without synthetic commands. Electron acknowledges only after the complete
  fork response and canonical raw child aggregate pass independent validation
  and are installed. A presentation acknowledgement is not approval,
  authorization, or proof the user saw the result. See
  [ADR 0017](../decisions/0017-durable-conversation-fork-delivery.md).
- IPC v11 removes public artifact create/update/delete requests and reserves
  their former tags. Get/List projections remain public; tested
  `WorkspaceService` and store mutations remain inward-facing for a future
  trusted ingestion producer. Legacy mutation tags decode to no operation and
  epochs 1 through 10 are rejected before dispatch. See
  [ADR 0018](../decisions/0018-remove-public-artifact-metadata-producer-authority.md).
- IPC v12 removes public generic message create/update/delete requests and
  reserves their former tags. Get/List projections remain public; typed
  daemon-owned Start, Retry, Branch, Edit-and-branch, and Regenerate commands
  remain the only public conversation producers. Legacy mutation tags decode
  to no operation, cannot change a seeded canonical message, and epochs 0
  through 11 are rejected before dispatch. See
  [ADR 0020](../decisions/0020-remove-public-message-mutation-authority.md).
- IPC v13 permanently removes `Artifact.relative_path` from the public wire
  projection. SQLCipher schema 16 transactionally replaces artifact search
  triggers, rebuilds existing cache rows with an empty body, and rebuilds FTS5;
  runtime SQL artifact matching is title-column-only even if its external index
  is later desynchronized. Memory and SQL search neither match path-only tokens
  nor return a path snippet, while the artifact display name remains searchable.
  Migration failure rolls back the
  cache and trigger changes and restarts from schema 15. Epochs 0 through 12
  are rejected. See
  [ADR 0021](../decisions/0021-remove-artifact-storage-path-projection.md).
- IPC v14 adds typed `ImportArtifact` and exact-version `OpenArtifact`
  operations. SQLCipher schema 17 owns pathless artifact lifecycle rows,
  immutable version digests, quota accounting, exact command resolution, and
  separate import/open journals. Legacy available metadata becomes unavailable
  until verified content is imported. Linux copies one no-follow regular file
  up to 64 MiB into owner-private storage, publishes with descriptor-relative
  no-replace rename, and opens only a revalidated read-only descriptor through
  the XDG desktop portal. Open dispatch interruption becomes
  `interrupted_needs_review` and is never replayed. Known failures project only
  a closed path-free failure code for specific Library recovery feedback.
  Electron main owns the file chooser; preload and renderer receive no selected
  path. Unsupported platforms and unsafe storage roots leave the Files
  capability unavailable. See
  [ADR 0022](../decisions/0022-daemon-owned-artifact-import-and-open.md) and
  [ADR 0023](../decisions/0023-linux-private-artifact-content-and-fd-open.md).
- IPC v15 adds exact daemon-owned `RemoveArtifact`; SQLCipher schema 18 adds
  immutable version-retention rows and an atomic removal command journal. One
  transaction records the command, tombstones the exact current artifact, and
  moves all retained versions to purge-pending. Count quota releases at
  tombstone, while byte quota remains charged until each version is durably
  absent from the private namespace and marked Purged. Same-key retry and
  startup recovery process bounded pages under a single-flight guard; a typed
  path-free pending receipt transfers post-reservation cleanup to one bounded
  daemon background task. Library treats that receipt as pending, retains its
  exact key through bounded presentation reconciliation, and accepts only an
  exact terminal replay as cleanup completion. Linux purge revalidates the deterministic entry's
  private owner, mode, link count, and device/inode identity before
  descriptor-relative unlink and directory sync. Content corruption does not
  make an otherwise qualified daemon-owned entry undeletable. The operation
  does not modify the original source, revoke descriptors already held by
  another application, or claim secure physical erasure. See
  [ADR 0024](../decisions/0024-daemon-owned-artifact-removal-and-retention.md).
- SQLCipher online backups on Linux retain validated parent, staging, and
  snapshot descriptors, finalize and verify a private encrypted snapshot in
  exact `DELETE` journal mode, reject destination sidecars, and publish with a
  handle-relative exclusive no-replace rename. The destination parent must be
  current-user-owned mode `0700` under protected ancestors; existing and raced
  targets are preserved. Reported rename errors are reconciled by retained
  source/target identity; an unprovable result is explicitly uncertain. This is
  an other-user and cooperative-publisher boundary, not a malicious-same-user
  guarantee. Apple, Windows, and other platforms fail closed pending native
  implementation and qualification. See
  [ADR 0019](../decisions/0019-private-atomic-backup-publication.md).
- Exact revisioned approval decisions are committed atomically with their run
  transition: grants resume execution, while denials and expired decisions
  pause the run instead of leaving it awaiting approval. Expiry is durably
  recorded before the deterministic error is returned, and exact replays
  preserve that outcome.
- The production artifact projection does not disclose daemon-relative or
  selected source paths at raw Protobuf response, Electron-main result,
  preload, renderer, persistence, or search boundaries. Available artifacts
  carry a distinct exact content version; unavailable and deleted projections
  cannot expose content metadata.
- Closed v1 desktop deep-link parsing, single-instance/cold-start/macOS
  activation delivery, preload readiness acknowledgement, and typed renderer
  routing. Raw activation URLs never enter renderer state, hidden windows are
  not restored for invalid scheme-like arguments, and the generated MSIX
  manifest binds one quoted URI argument to the packaged full-trust executable.
- Strict external source opening through Electron main only. The typed preload
  request accepts exact canonical credential-free public HTTPS DNS URLs from
  the trusted primary top-level frame, rejects local/private names and every IP
  literal, admits one shell launch at a time under a rolling rate bound, and
  returns only fixed failure codes. Citations stay inert until the user chooses
  **Open source in browser** in the source inspector.
- Capability-focused `IsolationProbe` application port and read-only
  `grok-vm-service-client` adapter. The Windows transport explicitly requests
  `SECURITY_IDENTIFICATION`, accepts only the fixed bounded capability response,
  and verifies the pipe server against the running LocalSystem SCM service,
  exact package identity, and packaged executable paths.
- Durable privileged-operation aggregate, capability-focused application store
  and coordinator, and memory/SQLCipher adapters with atomic intent/payload and
  dispatch/attempt persistence. Schema migration 7 establishes the journal;
  schema migration 11 hardens attempt epoch evidence by rejecting zero boot
  identities, non-positive attempt durations, and reuse of the durable journal
  ID as a transport ID. A bounded daemon startup pass moves interrupted
  retry-safe attempts to `retry_pending` and non-idempotent attempts to
  `interrupted_needs_review` without I/O, and fails startup closed if the bound
  is exceeded. There is still no privileged execution gateway, public IPC, or
  guest-control authority, so this exposes no executable Work path.
- Explicit MSIX inventory, signing, Electron fuse, provenance, architecture,
  and outer/inner artifact-binding checks for Windows x64 and ARM64.

## End-to-end workflows available in the current tree

- Durable local project, thread, message, and disabled automation-definition
  CRUD through the daemon, plus daemon-owned artifact import, exact-version
  local open, and confirmed local-copy removal on qualified Linux storage.
  Library gates these operations on the Files capability; import additionally
  requires the production Electron chooser and an active project. Export and
  richer content operations remain unavailable.
- Direct official xAI Chat from Electron through the daemon when a previously
  enrolled key and the product-selected model pass live discovery. Current IPC
  v15 retains epoch 7's immediate durable turn and normalized persisted text
  events. Completed and failed outcomes are restored from SQLCipher, exact Stop
  intent is revisioned and replayable, and safe Retry creates a new lineaged
  attempt from only the latest cancelled or retryable known-failed source while
  preserving its frozen provider request. Branch, Edit-and-branch, and
  Regenerate create explicit child threads with immutable fork-point lineage;
  only Edit/Regenerate initiate a newly confirmed provider request. Corrupt
  persisted aggregates fail
  rehydration, and uncertain provider dispatch is never replayed automatically.
  Bounded startup recovery resolves crash-left reservations and in-flight calls
  before the daemon accepts IPC.
- Native xAI key enrollment intent, non-secret vault status, and credential
  deletion. IPC v2 introduced the wire change; current IPC v15 rejects epochs 0
  through 14. The Wayland developer pass now covers the real protected pinentry,
  environment isolation, process-group cleanup, and Escape cancellation. The
  Win32 boundary still needs native Windows qualification, while the Linux
  pinentry path remains a developer workflow pending representative X11,
  alternate pinentry, packaged-desktop, and Secret Service qualification.
- Daemon and official Grok Build component health reporting.
- Limited Mode behavior when provider, subscription, managed browser, computer
  use, or qualified isolation facts are absent.
- Validated v1 desktop links can restore the existing single-instance window
  and select an allowlisted top-level, project, or conversation route. Signed,
  installed-MSIX activation on Windows x64 and ARM64 remains qualification
  evidence rather than a completed release workflow.
- Displayed citations can be opened explicitly in the operating-system browser
  through the bounded native URL broker; the renderer has no general external
  anchor, popup, or shell authority.

These workflows are engineering-preview behavior. The repository has no signed
release artifact and has not passed the release matrix.

## Linux full product GA blockers

Tracked against [linux-ga.md](linux-ga.md). Summary only; Windows-specific gates
remain under **Windows qualification blockers**.

| Train | Status |
| --- | --- |
| T0 Architecture + GA contract | **Done** (`linux-ga.md`, platform ADRs 0004–0007) |
| T1 Linux packaging / updater | **Done entry** (`pnpm package:linux` embeds daemon; no auto-updater) |
| T2 BYOK / pinentry / Files qualification | **Code + structural tests**; full packaged DE matrix still open |
| T3 Linux QEMU/KVM broker + virtio image | **Broker + unix socket main** exposes ensure_image/create_vm/start_vm/guest_control; StartVm requires Spawn (lab injects fake process). Wire: Go `[]byte` ↔ base64 JSON fixtures. Residual: production QEMU matrix |
| T4 Privileged gateway + PoP + live isolation facts | **IsolationRuntime** journals `runner.health`; Linux dialer **orchestrates** EnsureImage→Create/Start→grant→health. Peer: **SO_PEERCRED + /proc/pid/exe** (client peerExe not authoritative). Residual: real KVM+image release matrix |
| T5 Subscription host auth + guest ACP Work | **Host auth IPC + Setup Connect**; Work still needs isolation+auth facts |
| T6 Overlay host commit UX | Specified; not product-wired |
| T7 Automation execution | **Epoch 18** `schedule_active` + `KernelInitializedExecutionEnabled` when journal recovers; `execute_due` claims and links durable runs. Residual: full product schedule UX + occurrence history still thin |
| T8 Media / voice / search product ops | Capabilities unavailable; **Library files-only** (Imagine create tabs removed) |
| T9 Managed browser + Wisp lifecycle | **Signed host lifecycle**: fixture `integrations/testdata/wisp-signed` (Ed25519); daemon `ManagedIntegrationService` + epoch-19 get/change IPC; development source under `first-party/wisp` stays `algorithm: none`. Residual: guest `catalog.apply` dispatch when isolation is live |
| T10–T11 Policy settings, export, diagnostics | Unavailable labels; Settings copy marks unfinished rows |
| T12 Linux release matrix + evidence | Engineering evidence under goal scratch; full matrix open |

## Product blockers

- Chat still needs official thread/account identity when an official contract
  exposes it, provider continuation policy, real-key Linux/Windows
  qualification, and release-scale load/recovery evidence. Safe linear Retry
  plus explicit child-thread Branch/Edit/Regenerate are implemented;
  completed and uncertain turns remain ineligible for Retry. General
  background-operation event synchronization remains separate; IPC v4's
  run-event long poll carries audit events only, while current IPC (epoch 16)
  retains epoch 7's direct-Chat-only turn-local text stream.
- Subscription authentication and session execution need a daemon-owned ACP
  lifecycle. Session prompts remain guest-only and unavailable until the
  qualified guest proxy exists. On Linux that proxy is the future QEMU/KVM
  broker (platform ADR 0004), not host execution.
- BYOK add/replace needs native qualification of packaged process/window
  identity, prompt accessibility, cancellation, HWND reuse, buffer cleanup, and
  crash behavior on Windows. Linux additionally needs representative
  Wayland/X11 pinentry variants, prompt cancellation/process cleanup, and
  Secret Service qualification. The general renderer bridge cannot carry the
  key.
- Automations need a durable scheduler, lease/overlap/missed-run enforcement,
  history, sleep/reboot recovery, and multi-day soak evidence before an enabled
  schedule can be represented as active.
- Linux local artifact ingestion, durable ownership, and exact local-copy
  deletion recovery are implemented. Attachments still need qualified
  Windows/macOS content adapters, richer type validation and malware policy,
  provider upload lifecycle, export, and cross-platform qualification.
- Settings that affect approvals, network, retention, updates, accessibility,
  notifications, resources, and model selection need a versioned policy store
  and enforcement path before their controls can be enabled.
- Media, voice, integrations, updater, import/export, diagnostics, durable
  run/approval event recovery, and support tooling remain incomplete workflows.

## Windows qualification blockers

- The service-owned native HVSock connector, authenticated start/restart rekey,
  and typed named-pipe guest-control operation are implemented and
  cross-compile, but remain unadvertised and authorization-gated. Production
  exposes no raw guest socket.
- Named-pipe connections are bound per frame to SID, logon session, PID,
  process creation time, exact packaged daemon path, and the broker's own
  package full name/family. The read-only Rust probe independently qualifies
  the pipe server's SCM PID, LocalSystem configuration, package identity, and
  fixed path. These are static broker-readiness facts only. A proof-bearing
  daemon session, typed privileged gateway, and approval-bound dispatch path
  remain blockers. The implemented journal store and startup recovery perform
  no I/O and grant neither guest control nor Work; package/process
  qualification alone never grants either capability.
- The identification-only named-pipe boundary has not run from the exact signed
  MSIX on real Windows 11 x64 or ARM64. There is no released
  `SecurityImpersonation` compatibility mode; production must reject stronger
  token levels and fail closed if server identity cannot be established.
- The generated MSIX manifest contains the closed `grok-desktop` protocol
  registration, but activation through an actually signed and installed
  package has not run on Windows 11 x64 or ARM64.
- Real Windows 11 HCS, ACL, reparse-race, suspend/resume, restart, low-resource,
  install/update/repair/uninstall, MSIX signing, accessibility, and x64/ARM64
  matrices have not run for an exact release artifact.
- The isolated Grok-home ACL implementation cross-compiles for Windows x64 and
  ARM64, but its native sharing, owner-DACL, hard-link, and reparse tests still
  require the real Windows qualification workers.

See [release qualification](release-qualification.md) for the promotion gates
and [the platform threat model](../platform/threat-model.md) for failure and
attacker assumptions.
