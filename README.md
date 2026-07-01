# Grok Desktop

Grok Desktop is a Windows-first, local-first desktop workspace designed for
official Grok and SpaceXAI services through documented Grok Build and xAI API
contracts. The product architecture covers conversational work, durable
projects, agent tasks, files and artifacts, browser and computer automation,
media generation, voice, integrations, and local schedules behind explicit
security boundaries.

> Grok Desktop is an independent project and is not affiliated with, endorsed
> by, or sponsored by SpaceXAI. Grok, SpaceXAI, and xAI are trademarks of their
> respective owners. The application uses a custom project identity and
> official service contracts only.

## Authentication

- **Subscription:** the qualified product delegates OAuth and model sessions to
  the official Grok Build client through Agent Client Protocol (ACP).
- **BYOK:** a separately funded xAI API key enables documented direct APIs. It
  is not SuperGrok subscription credit and never enables another provider.

The general Electron renderer is not a credential-entry boundary. On Windows,
the audited adapter in
[ADR 0005](docs/decisions/0005-native-credential-enrollment.md) presents native
credential UI directly from the daemon after qualifying the packaged Electron
owner window. On Linux, the daemon speaks a bounded Assuan exchange with a
protected canonical pinentry process using a cleared, validated spawn
environment and process-group cleanup. The supervised IPC nonce uses a one-shot
stdin pipe, and Linux disables same-user daemon inspection before async startup.
In both cases the key stays outside renderer, preload, Electron-main, argv, and
environment state. The enrollment contract was
introduced in IPC v2; current IPC v15 retains it and rejects epochs 0 through 14.
Windows release
enablement remains blocked on the Windows 11 x64 and ARM64 matrix; Linux
pinentry is implemented and Wayland-tested for developer use but still needs
representative X11, alternate pinentry, packaged-desktop, and Secret Service
qualification before GA. Non-secret
vault status and key removal also use daemon-owned contracts.

For a previously enrolled and validated key, direct Chat now follows a durable
daemon-owned turn journal: provider dispatch is recorded before the request,
completed output and citations are committed atomically, and uncertain
in-flight requests require review instead of automatic replay. IPC v7 introduced
immediate durable reservation, normalized assistant-text events, bounded
replayable polling, and exact Stop; its schema-12 event journal remains the
stream boundary under current SQLCipher schema 18. IPC v8 adds explicit safe
Retry for only the latest cancelled or retryable known-failed attempt. The
daemon reloads the source's exact prompt, context, recorded model, and local
credential generation; none can be supplied by the renderer. Completed and
uncertain attempts remain non-retryable. See
[ADR 0015](docs/decisions/0015-safe-direct-chat-retry-lineage.md).

IPC v9 adds daemon-owned child-thread Branch, Edit-and-branch, and Regenerate.
Branch copies a completed response without provider work; Edit and Regenerate
are explicitly confirmed new billable requests using the source turn's exact
frozen context, recorded model, and current matching local credential
generation. SQLCipher schema 14 stores immutable parent/source/message
derivations, bounded family metadata, inherited completed-assistant outcomes,
and exact command replay. Parent history is never rewritten, and the local
generation binding is not presented as an official account identity. See
[ADR 0016](docs/decisions/0016-daemon-owned-conversation-forks.md).

IPC v10 adds daemon-owned result delivery for those fork commands. A newly
committed child has a pending delivery in SQLCipher schema 15. A renderer
restart can bind a new bounded alias for the same unresolved request and recover
the canonical child without another provider dispatch; after strict canonical
validation and in-memory installation, the renderer acknowledges the exact
delivery revision. See
[ADR 0017](docs/decisions/0017-durable-conversation-fork-delivery.md).

IPC v11 removes generic artifact create/update/delete requests from public
desktop IPC and permanently reserves their former tags. Read-only artifact
projections remain available, while the tested application/store mutations stay
inward-facing for a future daemon-owned ingestion producer. See
[ADR 0018](docs/decisions/0018-remove-public-artifact-metadata-producer-authority.md).

IPC v12 removes generic message create/update/delete requests and permanently
reserves their former tags. Read-only message projections remain available;
typed daemon-owned start, Retry, Branch, Edit-and-branch, and Regenerate
commands remain the only public conversation producers. See
[ADR 0020](docs/decisions/0020-remove-public-message-mutation-authority.md).

IPC v13 removes the daemon-relative storage path from raw artifact responses
and permanently reserves its former field. SQLCipher schema 16 also rebuilds
artifact search rows with an empty body, so path-only tokens are neither
returned nor searchable while artifact display names remain indexed. See
[ADR 0021](docs/decisions/0021-remove-artifact-storage-path-projection.md).

IPC v14 adds the first daemon-owned artifact producer: one bounded regular-file
import into an immutable content version, plus exact-version local opening.
SQLCipher schema 17 persists import/open intent and terminal results, enforces
global and project quotas, and migrates legacy metadata to unavailable rather
than asserting unverified content. On Linux, private descriptor-relative
storage publishes with no-replace rename and local open hands a validated
read-only file descriptor to the XDG desktop portal. Other platforms and unsafe
storage roots fail closed. Electron owns the one-file chooser, but selected
paths are ephemeral request data and never enter responses, renderer state, or
durable storage. See
[ADR 0022](docs/decisions/0022-daemon-owned-artifact-import-and-open.md) and
[ADR 0023](docs/decisions/0023-linux-private-artifact-content-and-fd-open.md).

IPC v15 and SQLCipher schema 18 add exact daemon-owned removal of the local
imported copy without restoring the retired generic delete producer. One atomic
reservation records the command, tombstones the artifact, and moves all
retained versions to purge-pending. Count quota releases at tombstone; byte
quota releases only after each deterministic private namespace entry is durably
absent. Same-key retry, bounded-page startup recovery, and a typed path-free
pending receipt keep cleanup live across timeouts and renderer restarts. Linux
purge revalidates the private entry's owner, mode, link count, and file identity
before descriptor-relative unlink, so corrupt owned bytes remain removable.
Library reports pending cleanup separately and retains the exact replay key
until the daemon returns terminal proof.
Removal does not modify the original source or revoke descriptors already held
by another application. See
[ADR 0024](docs/decisions/0024-daemon-owned-artifact-removal-and-retention.md).

IPC v5 adds live official xAI model discovery and a daemon-owned default Chat
selection. A unique advertised alias is canonicalized before schema-9
persistence; missing, ambiguous, or explicitly non-text selections fail closed
without silently changing the model used for a new turn.

IPC v6 introduced bounded daemon-owned workspace search routing to canonical
conversations and removed generic run/approval producer mutations from public
IPC. Current IPC v15 retains those boundaries and the epoch-7 asynchronous
start, exact cancellation, and turn-local event polling contract while adding
daemon-owned Retry and child-thread fork lineage.
Renderers may read audit events and decide an exact pending
approval, but cannot create runs, choose lifecycle states, or author approval
disclosures.

No third-party model provider, arbitrary compatible endpoint, private Grok web
API, or imported browser cookie is supported.

The verified surface contracts and the controls required to keep the official
Grok Build CLI from inheriting custom providers are documented in
[docs/research/official-grok-surfaces.md](docs/research/official-grok-surfaces.md).

## Architecture

The Electron/React application is a sandboxed presentation shell. A separate
Rust daemon is the authority for domain rules, encrypted persistence, provider
integrations, approvals, scheduling, MCP, and worker orchestration; incomplete
engines remain unavailable rather than moving that authority into Electron.
The qualified Strong Work design uses a brokered Linux utility VM managed by a
narrow privileged Windows service. Current builds remain fail-closed in Limited
Mode until that path passes the Windows matrix.

See [AGENTS.md](AGENTS.md) for module boundaries and
[docs/architecture/overview.md](docs/architecture/overview.md) for the system
design. The verified Cowork reference points and the places where this project
deliberately differs are recorded in
[docs/research/claude-cowork-windows.md](docs/research/claude-cowork-windows.md).
The fail-closed MSIX layout, signing boundary, Electron fuses, and packaged
service constraints are documented in
[docs/platform/windows-release.md](docs/platform/windows-release.md).

## Development

There are no packaged releases yet; the only way to run Grok Desktop is from
source.

Prerequisites:

- Node.js >= 22.22 with corepack enabled (the repo pins `pnpm@10.33.2`)
- Rust 1.95+ (workspace edition 2024)
- Go, for `native/windows-vm-service` and `guest/runner`
- Nix (optional), for the reproducible guest image and `nix flake check`

```sh
pnpm install --frozen-lockfile

pnpm dev        # Vite + Electron dev shell
pnpm dev:web    # browser-only renderer preview (explicit, sample data)
pnpm dev:cdp    # persistent QA profile with CDP (see apps/desktop/scripts/README.md)

pnpm check      # lint + typecheck + test + build + Rust fmt/clippy/test
```

Individual gates: `pnpm lint`, `pnpm typecheck`, `pnpm test`, `pnpm build`,
`pnpm check:rust`, `pnpm test:e2e:electron`. Windows packaging is
`pnpm package:windows`; HCS-dependent tests require the documented Windows
qualification workers.

## Repository status

The product is under active construction. Public releases will be cut only
after the complete security, accessibility, Windows qualification, packaging,
and recovery matrices pass; intermediate engineering stages are not presented
as reduced product editions. The exact implemented surfaces and remaining
workflow blockers are tracked in
[docs/quality/implementation-status.md](docs/quality/implementation-status.md).

## Contributing

Outside code contributions are not accepted; see
[CONTRIBUTING.md](CONTRIBUTING.md). Issues for reproducible defects,
accessibility problems, hardening ideas, and design feedback are welcome.
Report suspected vulnerabilities privately per [SECURITY.md](SECURITY.md).

## Licensing

Copyright (c) 2026 Grok Insider. Grok Desktop is available under the GNU Affero
General Public License v3.0 or later, or under a separate commercial license.
See [LICENSE](LICENSE) and [COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md).
