# Protocol and persistence

Chronological reference for desktop IPC epochs, SQLCipher schemas, and the
durable contracts they carry. Stable process architecture lives in
[overview.md](overview.md). Layering rules live in [principles.md](principles.md).
Decision detail lives in the linked ADRs.

**Current surface (maintain as you bump):** desktop IPC **v29** rejects epochs
**0–28** before dispatch. SQLCipher production schema is **27**. Schema 25 adds
immutable Work backend/run classification and daemon-owned Host Tools policy;
schema 26 adds the restartable enrollment command journal; schema 27 and epoch
29 add the daemon-owned stable/beta update-channel preference. Epochs 27–28 add
durable Host Work start/cancel and bounded run/approval snapshots. Earlier
sections below describe contracts retained under later epochs. Always confirm
against `grok-protocol` / `grok-sqlcipher` when changing code.

No third-party model provider, arbitrary compatible endpoint, private Grok web
API, or imported browser cookie is supported. Official surface research:
[official-grok-surfaces.md](../research/official-grok-surfaces.md).

## Authentication paths

- **Grok Build subscription:** OAuth and model sessions via the official Grok
  Build client (Agent Client Protocol).
- **SuperGrok API Chat:** fresh official xAI device OAuth with `api:access`,
  daemon-vault ownership, and fixed `api.x.ai` traffic. It is not Build ACP or
  Grok product-chat traffic.
- **BYOK:** user-owned xAI API key for documented direct APIs. Not SuperGrok
  subscription credit; never enables another provider.

The Electron renderer is not a credential-entry boundary. Keys stay outside
renderer, preload, Electron-main, argv, and environment state. See
[ADR 0005](../decisions/0005-native-credential-enrollment.md).

## IPC and schema index

| Topic | IPC / schema | Primary ADRs |
|-------|--------------|--------------|
| System shell + daemon SoR | — | [0001](../decisions/0001-system-architecture.md) |
| Grok-only providers | — | [0002](../decisions/0002-grok-only-integrations.md) |
| Managed execution / Limited Mode | — | [0003](../decisions/0003-managed-execution.md) |
| Explicit HostDirect / IsolatedGuest Work backends | epoch 25 / schema 25 | [0032](../decisions/0032-explicit-dual-mode-work-execution.md) |
| Host enrollment command replay journal | schema 26 | [0032](../decisions/0032-explicit-dual-mode-work-execution.md) |
| Durable Host Work start/cancel and run snapshots | epochs 27–28 | [0032](../decisions/0032-explicit-dual-mode-work-execution.md) |
| Credentials + capabilities | — | [0004](../decisions/0004-daemon-owned-credentials-and-capabilities.md) |
| Native credential enrollment | IPC v2+ (retained in v15) | [0005](../decisions/0005-native-credential-enrollment.md) |
| Durable direct Chat turns | schema-backed journal | [0006](../decisions/0006-durable-direct-chat-turns.md) |
| Desktop preferences and signed update channel | IPC v29, schema 27 | [0007](../decisions/0007-daemon-owned-desktop-preferences.md), [0030](../decisions/0030-signed-public-update-channels.md) |
| Resumable run-event long poll | IPC v4 | [0008](../decisions/0008-resumable-run-event-long-poll.md) |
| xAI Chat model selection | IPC v5, schema 9 | [0009](../decisions/0009-daemon-owned-xai-chat-model-selection.md) |
| Workspace search routing | IPC v6, schema 10 | [0010](../decisions/0010-daemon-owned-workspace-search-routing.md) |
| Remove renderer run/approval producers | IPC v6+ | [0011](../decisions/0011-remove-renderer-execution-producer-authority.md) |
| Deep links | — | [0012](../decisions/0012-versioned-desktop-deep-links.md) |
| Async Chat events + exact cancel | IPC v7, schema 12 journal | [0013](../decisions/0013-durable-async-chat-events.md) |
| External URL broker | — | [0014](../decisions/0014-strict-external-url-broker.md) |
| Safe Retry lineage | IPC v8, schema 13 lineage | [0015](../decisions/0015-safe-direct-chat-retry-lineage.md) |
| Conversation forks | IPC v9, schema 14 | [0016](../decisions/0016-daemon-owned-conversation-forks.md) |
| Fork delivery | IPC v10, schema 15 | [0017](../decisions/0017-durable-conversation-fork-delivery.md) |
| Remove artifact metadata producers | IPC v11 | [0018](../decisions/0018-remove-public-artifact-metadata-producer-authority.md) |
| Private atomic backup | — | [0019](../decisions/0019-private-atomic-backup-publication.md) |
| Remove message mutation producers | IPC v12 | [0020](../decisions/0020-remove-public-message-mutation-authority.md) |
| Remove artifact path projection | IPC v13, schema 16 | [0021](../decisions/0021-remove-artifact-storage-path-projection.md) |
| Artifact import + open | IPC v14, schema 17 | [0022](../decisions/0022-daemon-owned-artifact-import-and-open.md), [0023](../decisions/0023-linux-private-artifact-content-and-fd-open.md) |
| Artifact removal + retention | IPC v15, schema 18 | [0024](../decisions/0024-daemon-owned-artifact-removal-and-retention.md) |
| Automation scheduler journal | — | [0025](../decisions/0025-daemon-owned-automation-scheduler-journal.md) |
| SuperGrok API Chat rail | IPC v21, schema 20 lineage | [0026](../decisions/0026-daemon-owned-supergrok-api-chat-rail.md) |
| Atomic scheduled dispatch | schema 21 | [0025](../decisions/0025-daemon-owned-automation-scheduler-journal.md) |
| Durable signed managed integrations | schema 22 | [0027](../decisions/0027-durable-signed-managed-integration-lifecycle.md) |
| Local completed-turn usage summary | IPC v23, schema 23 | [0029](../decisions/0029-local-usage-summary-ipc.md) |
| Durable official xAI Search grant | IPC v24, schema 24 | [0031](../decisions/0031-durable-official-xai-search.md) |
| Privileged guest / channel / journal | platform ADRs | [platform/adr](../platform/adr/) |

## Narrative detail

The following sections preserve the durable technical narrative formerly
embedded in the architecture overview and root README. Prefer ADRs when
implementing; use this file for cross-epoch orientation.

## Windows broker, isolation probe, and enrollment

On Windows, the LocalSystem broker owns HCS and every HVSock handle. VM start
includes a fresh authenticated guest-channel handshake, and service restart
must rekey an adopted runtime or stop it. The narrow `guest_control` proxy is
present but remains unavailable to production callers until packaged daemon
qualification is followed by proof-of-possession and durable replay recovery;
no caller receives a raw guest endpoint.

The application-layer `IsolationProbe` port and
`crates/grok-vm-service-client` adapter expose only a bounded
`get_capabilities` probe. On Windows the adapter requests
`SECURITY_IDENTIFICATION`, then verifies the pipe server against the running
LocalSystem SCM service, its exact packaged own-process configuration, exact
package identity, and fixed packaged executable layout before accepting the
response. Its result is a static broker
compatibility fact. It does not prove guest health, daemon possession of an
execution credential, approval, or durable side-effect recovery, and therefore
cannot make Work or `guest_control` available.

Credential enrollment is also outside the general renderer. On Windows, an
audited adapter presents masked Win32 credential UI inside the daemon after
verifying the exact packaged daemon layout and the owning Electron window's
executable and MSIX identity. On Linux, the daemon drives a local pinentry
process over a bounded Assuan exchange, using a protected canonical executable,
a cleared and validated display/session/locale environment, and process-group
cleanup. The supervised IPC nonce crosses a one-shot stdin pipe rather than the
daemon or pinentry environment; Linux disables same-user process inspection
before the async runtime starts. Electron receives only non-secret account
state on either path. The enrollment intent was introduced in IPC v2; current IPC v28 carries it
and rejects epochs 0 through 14 before dispatch. Release enablement remains
fail-closed until the Windows native
matrix and the Linux desktop, pinentry, cancellation, and Secret Service matrix
pass. The boundary is specified in
[ADR 0005](../decisions/0005-native-credential-enrollment.md).

## Durable direct Chat

Direct BYOK Chat is coordinated as a durable daemon aggregate. User intent and
the immutable provider context are reserved before dispatch; a provider-started
request is never automatically replayed after an uncertain interruption; and a
completed assistant message, citations, usage, and run state commit together.
Persisted turns are accepted only through `ConversationTurn::restore`, which
revalidates bounded non-secret metadata and the complete reachable-state matrix:
dispatch evidence, result/certainty fields, revision, and timestamps must agree
with `reserved`, `provider_started`, or the exact terminal state. In particular,
a cancelled turn cannot contain provider-dispatch evidence, and an uncertain
turn cannot be restored as replayable.

Every terminal transition uses a durable timestamp floor: wall time is raised
to at least the latest turn, run, and effect timestamp before the atomic commit.
Provider output must also fit the canonical and storage representations. A turn
may retain at most 256 citations, each title is at most 500 bytes, each HTTPS URL
is at most 8,192 bytes, their aggregate title/URL content is at most 1,000,000
bytes, citation URLs must parse with a nonempty HTTPS host and no credentials,
and every usage counter must fit JavaScript's exact safe-integer range across
Rust, Protobuf, Electron, and SQLCipher. Malformed post-dispatch output or a
rejected atomic terminal commit is classified through an optimistic
`interrupted_needs_review` fallback instead of being retained as a known
result. A completed xAI Responses event must carry the exact embedded response
object/status, a bounded response identifier, and nonnegative integer input,
output, and cost-tick accounting; missing or malformed terminal accounting is
outcome-unknown rather than fabricated as zero. Authoritative incomplete token
limits become known non-retryable invalid requests, the provider time limit is
a known retryable unavailable result, and unknown incomplete reasons fail with
sanitized local protocol copy. Success and known failure remain buffered until
`[DONE]` or a clean SSE EOF; a duplicate/opposite terminal, ignored or unknown
event, delta, decoder residue, or transport failure after a terminal
observation makes the outcome uncertain. Citations are accepted only from the
official message/output-text annotation fields and exact streaming annotation
events; recursive matches inside untrusted tool output are ignored, malformed
recognized citations fail closed, and a 257th unique URL is never silently
truncated. Image generation likewise requires explicit provider cost
accounting instead of inventing a zero. If a terminal
commit wins but its acknowledgement is ambiguous, the service reloads the
durable terminal winner. Later turns exclude prompts owned
by cancelled, failed, incomplete, or review-required turns from provider
context, so a new message cannot implicitly redisclose unresolved input.

SQLCipher schema 13 gives each direct-Chat turn immutable original-or-retry
lineage and binds a thread once to the daemon-local credential enrollment
generation used by its first turn. That generation identifier is neither an
official xAI account identity nor derived from key bytes, and it never crosses
renderer IPC. Re-enrollment requires a new thread. Migrated threads with
historical turns remain readable but unbound and reject later Start or Retry

## Conversation forks and delivery

IPC v9 and SQLCipher schema 14 extend that model with daemon-owned conversation
forks. Branch copies frozen context plus a completed assistant into a new child
without provider work. Edit-and-branch replaces only the copied final user
entry, while Regenerate copies through that user entry; both reserve a new
billable child turn using the source turn's recorded model and current matching
local credential generation. Thread lineage binds root, parent, source turn,
source message, kind, and depth. Every copied message has a child-owned identity
and immutable source derivation; inherited completed-assistant citations and
usage are projected separately without cloning provider outcome identity.
Direct children, family size, and depth are bounded and transactionally
revalidated. Inherited metadata is limited to 256 outcomes and a conservative
3 MiB before child commit and on every load; SQLCipher proves copied-assistant
ancestry with a visited, maximum-65-edge traversal rather than recursively
materializing prior fork snapshots under its connection lock. Parent history is
never mutated, and the local generation binding is still not an official
account identity.

IPC v10 and SQLCipher schema 15 add daemon-owned fork-result delivery without
moving intent into renderer storage. Each new fork transaction creates one
Pending/revision-0 delivery. Exact keys replay first; a different key with the
same pending operation fingerprint is atomically retained as one of at most 64
immutable aliases and returns the existing child. Only after the renderer has
validated and installed the canonical child does it acknowledge the delivery
to revision one. Acknowledged fingerprints are released for a later deliberate
fork, while exact canonical and alias keys keep replaying their original child.
The acknowledgement is a presentation handoff marker, not authorization,
approval, or proof that a person viewed the result. See
[ADR 0017](../decisions/0017-durable-conversation-fork-delivery.md).

## History bounds and startup recovery

Conversation history materializes at most one result plus one look-ahead row,
regardless of the caller's nominal page request, so maximum-size turns cannot
create a 100–200 MiB response graph before encoding. The full Protobuf envelope
is then checked against the exact 4 MiB local IPC frame limit and retains a
resumable cursor; one turn at every canonical metadata and message bound fits
that frame. Provider-context capture likewise preflights or incrementally
enforces its 1,000-message and 2 MiB limits before cloning row sets.

Before accepting IPC, daemon startup resolves at most 100 incomplete turns
without provider I/O. Reserved turns are cancelled; provider-started turns move
to `interrupted_needs_review`. An exact-100 backlog completes and serves. A
101-entry backlog commits the first bounded pass, fails startup before IPC, and
is completed by a later launch.

## Long poll, async Chat events, Retry

stream on dedicated Electron connections. IPC v7 separately gives each direct
Chat turn a normalized schema-12 event journal: `Created`, exact lifecycle
edges, and bounded UTF-8 text appends with contiguous sequence and offset
validation. Start returns the durable reservation before provider completion;
dedicated poll clients advance only after trusted renderer acknowledgement and
retain their cursor across daemon reconnects. Exact cancellation commits
`cancelled` before dispatch or `interrupted_needs_review` after dispatch before
the daemon signals its bounded provider task. `Envelope.Event` remains unused,
and this channel is not a general background-operation producer. See
[ADR 0006](../decisions/0006-durable-direct-chat-turns.md) and
[ADR 0008](../decisions/0008-resumable-run-event-long-poll.md), and
[ADR 0013](../decisions/0013-durable-async-chat-events.md).

IPC v8 retains that asynchronous contract and adds explicit safe Retry
at request field 49. The renderer sends only an exact source turn and revision;
the daemon reuses the source's frozen prompt, provider context, recorded model,
and local credential generation. Only the latest cancelled or retryable
known-failed source with no retry child is eligible. Completed, active,
non-retryable, uncertain, legacy-unbound, or credential-generation-mismatched
sources fail closed before provider dispatch; archived ownership and exhausted
depth have explicit actionless reasons. Lineage seals the immutable context,
and credential read leases prevent torn key/binding generations or dispatch
after a winning replacement/deletion. See
[ADR 0015](../decisions/0015-safe-direct-chat-retry-lineage.md).
Current IPC v15 retains those guarantees, the child-thread fork model described
above and in [ADR 0016](../decisions/0016-daemon-owned-conversation-forks.md),
and the durable presentation handoff in
[ADR 0017](../decisions/0017-durable-conversation-fork-delivery.md).

## Model catalog and workspace search

IPC v5 discovers the credential-scoped model catalog through the same fixed
official xAI adapter and stores one revisioned canonical default in SQLCipher
schema 9. Capability resolution checks that persisted selection against a live
text-capable descriptor; it never substitutes another model. See
[ADR 0009](../decisions/0009-daemon-owned-xai-chat-model-selection.md).

IPC v6 exposes bounded canonical workspace search with owning-thread
routing and removes generic run-state/approval producer mutations from public
IPC. SQLCipher schema 10 rebuilds the derived search cache from canonical
project, thread, message, artifact, and automation rows so stale, forged, or
orphaned cache entries cannot become search results. See
[ADR 0010](../decisions/0010-daemon-owned-workspace-search-routing.md) and
[ADR 0011](../decisions/0011-remove-renderer-execution-producer-authority.md).

## Artifacts: paths, producers, import, open, removal

IPC v13 reserves the former daemon-relative artifact path field, so storage
paths do not enter Electron main, preload, renderer contracts, or search.
SQLCipher schema 16 transactionally rebuilds artifact search documents with an
empty body and runtime matching remains title-column-only. Display names stay
searchable while path-only tokens are neither matched nor returned. See
[ADR 0021](../decisions/0021-remove-artifact-storage-path-projection.md).

IPC v11 removes generic artifact create/update/delete operations from the
public desktop protocol and reserves their former tags. Get/List projections
remain available, and no renderer-connected client may directly assert
available content, paths, types, sizes, or lifecycle state. See
[ADR 0018](../decisions/0018-remove-public-artifact-metadata-producer-authority.md).

IPC v12 also removes generic message create/update/delete operations and
reserves their former tags. Get/List projections remain public, but renderer
clients cannot manufacture system/user/assistant history or bypass typed turn,
Retry, and child-thread fork commands. Inward message-store operations remain
available only to those daemon-owned producers. See
[ADR 0020](../decisions/0020-remove-public-message-mutation-authority.md).

IPC v13 removes the artifact storage-path projection itself and permanently
reserves its former field. Schema 16 removes that path from both new and
existing artifact search documents, rebuilds FTS5, and restricts artifact
matching to titles, including the query-oracle channel left by merely hiding
snippets.

IPC v14 adds a narrow typed producer for import and exact-version local open.
SQLCipher schema 17 replaces legacy artifact rows with pathless lifecycle rows,
immutable digest-addressed versions, and separate import/open journals. Legacy
`available` metadata migrates to `unavailable`; content becomes `available`
only after the daemon copies, hashes, validates, publishes, and commits an exact
version. Intent is resolved before quotas or current lifecycle checks, so an
exact command retry cannot repeat source access or platform open. A crash after
open dispatch becomes `interrupted_needs_review` and is never replayed.

On Linux, the content adapter retains owner-private directory descriptors,
copies only regular no-follow sources up to 64 MiB into `0600` staging, checks
source identity and digest, and publishes immutable objects with a
descriptor-relative no-replace rename. Opening revalidates the exact object and
passes a read-only descriptor to the XDG desktop portal. Paths are never
returned or persisted. Unsafe roots, unsupported platforms, integrity failures,
and unavailable portals fail closed. Electron main owns a bounded one-file
chooser; the renderer receives only the canonical daemon result. See
[ADR 0022](../decisions/0022-daemon-owned-artifact-import-and-open.md) and
[ADR 0023](../decisions/0023-linux-private-artifact-content-and-fd-open.md).

IPC v15 and schema 18 add the closed `RemoveArtifact` command without
reintroducing generic artifact mutation. An exact idempotency key, artifact,
revision, and current content version reserve one atomic tombstone plus
`Retained -> PurgePending` transition for every immutable version. New opens
stop immediately, artifact-count quota releases at tombstone, and byte quota
remains charged until each version is durably absent from the private storage
namespace and marked `Purged`. A command commits only after all versions are
purged. Pending work resumes under one application single-flight guard in
bounded pages; timeout and renderer uncertainty retain the same exact command,
and a path-free pending receipt transfers cleanup to bounded daemon background
recovery. The renderer keeps that pending command identity through bounded
same-key reconciliation; a Deleted snapshot alone does not prove cleanup
commit.

Linux removal derives the object entry from canonical identity, reopens it
relative to retained private directory descriptors, and revalidates owner,
`0600` regular-file mode, single-link status, and device/inode identity before
unlink and directory sync. It intentionally does not require content digest
agreement, so a corrupt daemon-owned object can be reclaimed without granting
an arbitrary path-delete primitive. Namespace deletion does not revoke a
descriptor another application already holds or promise immediate physical
block reclamation. Storage readiness is now independent from portal-open
readiness, allowing import/removal recovery while OpenURI is unavailable. See
[ADR 0024](../decisions/0024-daemon-owned-artifact-removal-and-retention.md).

## Encrypted-memory policy

The daemon leaves SQLCipher's process-global `cipher_memory_security` allocator
disabled. SQLCipher 4.12 applies the option to every SQLite allocation, cannot
disable it after enabling it, and continues with pageable memory after
`mlock`/`VirtualLock` quota failures. Grok Desktop does not report that
best-effort behavior as successful memory locking. Database keys remain
daemon-owned and zeroized by the Rust key boundary, while SQLCipher continues
to wipe its dedicated key allocations. Database-at-rest encryption remains
mandatory on every platform.

## Backup publication

SQLCipher backup publication is likewise fail-closed. Linux retains validated
parent, staging, and snapshot descriptors inside a current-user-owned `0700`
destination parent with protected ancestors. It finalizes the snapshot to
`DELETE` journal mode, verifies it without the normal WAL-enabling opener,
rechecks namespace identities and destination sidecars, and commits with a
handle-relative exclusive no-replace rename. A reported rename error is
reconciled from retained source/target identities; an unprovable result becomes
explicitly uncertain instead of retryable. This protects against other users
and cooperative concurrent publishers; it does not claim integrity against a
malicious process running as the same OS user. Apple, Windows, and other
platforms report the operation unavailable pending native implementation and
qualification; path-based replacement is not a fallback. See
[ADR 0019](../decisions/0019-private-atomic-backup-publication.md).

## Deep links and external URL broker

Operating-system activation links are parsed only in Electron main under the
closed v1 `grok-desktop://open/` contract. The renderer receives a typed internal
route only after its isolated preload listener is ready; raw links never enter
renderer state. Main retains the latest typed route until the trusted preload
acknowledges delivery and restores a tray-hidden window only for a valid
activation. The MSIX template registers the closed scheme with one quoted URI
argument, but signed installed-package activation remains Windows
qualification evidence. See
[ADR 0012](../decisions/0012-versioned-desktop-deep-links.md).

Renderer-initiated source opening uses a separate Electron-main contract. The
renderer exposes no external `href`, `window.open`, or shell primitive: an
explicit source-inspector button sends one typed URL request through preload.
Main revalidates the trusted primary top-level application frame, accepts only
exact canonical credential-free public HTTPS DNS URLs, rejects every IP
literal and local/private name suffix, and bounds concurrent and rolling shell
launches. Native failures are reduced to fixed result codes and never retried;
the renderer sandbox, CSP, popup denial, and in-app navigation allowlist remain
unchanged. See
[ADR 0014](../decisions/0014-strict-external-url-broker.md).


## Privileged-operation journal (internal)

Capability-focused store and coordinator, memory and SQLCipher adapters, exact
idempotency replay, atomic intent/payload and dispatch/attempt commits,
validated rehydration, and bounded daemon startup recovery. Retry-safe
interrupted attempts become `retry_pending`; non-idempotent attempts become
`interrupted_needs_review` with no recovery I/O. See
[platform ADR 0003](../platform/adr/0003-durable-privileged-operation-journal.md).

There is still no typed privileged execution gateway, proof-bearing daemon
session, public IPC for the journal, or production guest-control caller. Work
remains fail-closed in Limited Mode until the complete Windows qualification
path exists.

## Related

- [Implementation status](../quality/implementation-status.md)
- [Release qualification](../quality/release-qualification.md)
- [ADR index](../decisions/README.md)
