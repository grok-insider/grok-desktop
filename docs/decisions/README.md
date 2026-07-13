# Architecture decision records

Product-level ADRs for Grok Desktop. Platform guest/HCS decisions live under
[../platform/adr/](../platform/adr/).

## When to write an ADR

Add an ADR when a change:

- Alters public IPC, integration manifests, or recovery semantics
- Moves authority between renderer, daemon, service, or guest
- Introduces a new trust boundary or weakens an existing one
- Chooses a non-obvious alternative that future agents must not reverse casually

Small internal refactors that preserve contracts do not need an ADR.

## Template

```markdown
# ADR NNNN: Short title

- Status: proposed | accepted | superseded by ADR NNNN
- Date: YYYY-MM-DD

## Context

## Decision

## Consequences

## Rejected alternatives
```

Number sequentially. Link the ADR from
[protocol-and-persistence.md](../architecture/protocol-and-persistence.md)
when it defines an IPC or schema epoch.

## Index

| ADR | Title |
|-----|-------|
| [0001](0001-system-architecture.md) | Electron shell with a Rust system-of-record daemon |
| [0002](0002-grok-only-integrations.md) | Grok-only provider model |
| [0003](0003-managed-execution.md) | Fail-closed managed execution on Windows |
| [0004](0004-daemon-owned-credentials-and-capabilities.md) | Daemon-owned credentials and capability truth |
| [0005](0005-native-credential-enrollment.md) | Native credential enrollment boundary |
| [0006](0006-durable-direct-chat-turns.md) | Durable direct Chat turns |
| [0007](0007-daemon-owned-desktop-preferences.md) | Daemon-owned desktop behavior preferences |
| [0008](0008-resumable-run-event-long-poll.md) | Resumable long polling for durable run events |
| [0009](0009-daemon-owned-xai-chat-model-selection.md) | Daemon-owned xAI Chat model discovery and selection |
| [0010](0010-daemon-owned-workspace-search-routing.md) | Daemon-owned workspace search routing |
| [0011](0011-remove-renderer-execution-producer-authority.md) | Remove renderer execution-producer authority |
| [0012](0012-versioned-desktop-deep-links.md) | Versioned desktop deep-link activation |
| [0013](0013-durable-async-chat-events.md) | Durable asynchronous Chat events and exact cancellation |
| [0014](0014-strict-external-url-broker.md) | Strict external-URL broker |
| [0015](0015-safe-direct-chat-retry-lineage.md) | Safe direct-Chat Retry lineage |
| [0016](0016-daemon-owned-conversation-forks.md) | Daemon-owned conversation forks |
| [0017](0017-durable-conversation-fork-delivery.md) | Durable conversation-fork result delivery |
| [0018](0018-remove-public-artifact-metadata-producer-authority.md) | Remove public artifact-metadata producer authority |
| [0019](0019-private-atomic-backup-publication.md) | Private atomic backup publication |
| [0020](0020-remove-public-message-mutation-authority.md) | Remove public generic message mutation authority |
| [0021](0021-remove-artifact-storage-path-projection.md) | Remove artifact storage paths from projections and search |
| [0022](0022-daemon-owned-artifact-import-and-open.md) | Daemon-owned artifact import and exact-version open |
| [0023](0023-linux-private-artifact-content-and-fd-open.md) | Linux private artifact content and descriptor-based local open |
| [0024](0024-daemon-owned-artifact-removal-and-retention.md) | Daemon-owned artifact removal and retention |
| [0025](0025-daemon-owned-automation-scheduler-journal.md) | Daemon-owned automation scheduler journal |
| [0026](0026-daemon-owned-supergrok-api-chat-rail.md) | Daemon-owned SuperGrok API Chat rail |
| [0027](0027-durable-signed-managed-integration-lifecycle.md) | Durable signed managed-integration lifecycle |
| [0028](0028-durable-conversation-model-binding.md) | Durable per-conversation model binding |
| [0029](0029-local-usage-summary-ipc.md) | Local usage summary IPC |
| [0030](0030-signed-public-update-channels.md) | Signed public update channels |
| [0031](0031-durable-official-xai-search.md) | Durable official xAI Search grants |
| [0032](0032-explicit-dual-mode-work-execution.md) | Explicit dual-mode Work execution |
| [0033](0033-source-pinned-official-grok-build-component.md) | Source-pinned official Grok Build component |

## Platform ADRs

| ADR | Title |
|-----|-------|
| [platform 0001](../platform/adr/0001-privileged-guest-contract.md) | Narrow privileged guest and integration contracts |
| [platform 0002](../platform/adr/0002-authenticated-guest-channel.md) | Service-mediated authenticated guest channel |
| [platform 0003](../platform/adr/0003-durable-privileged-operation-journal.md) | Durable privileged-operation journal |
