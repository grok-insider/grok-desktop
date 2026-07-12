# Audited recovery and completion plan

- Snapshot: 2026-07-12
- Reviewed baseline: `66787d5ef3fe4bd23aadb49951b8d347ab32e8b6`
- Review range: `226484610d5ea4ecbb1c614cc7a741a2d054fc65..HEAD`
- Wire/schema implemented in the current tree: IPC epoch 23, SQLCipher schema 23
- Release status: not qualified for distribution

This directory is the canonical handoff for continuing the current work. It
does not turn a code path into a supported product capability. When this plan
and an older working note disagree, this plan and the repository invariants in
`AGENTS.md` win.

## Status vocabulary

- **Verified/completed**: source was inspected and relevant evidence previously
  exists. It still needs regression under the current tree before release.
- **Implemented, unsafe**: code exists but must remain unavailable until the
  listed corrective acceptance criteria pass.
- **Dirty/unreviewed**: present only in the worktree and not part of HEAD. It
  must not be committed as one unit or described as complete.
- **External gate**: cannot be honestly closed by local mocks or compilation.

## Current baseline

The repository contains substantial daemon-owned foundations: durable direct
xAI-key Chat, artifact import/open/removal, encrypted persistence, capability
resolution, official Grok Build ACP boundaries, tray/close-to-tray behavior,
and platform qualification scaffolding. The commits under review also add
useful Linux packaging/broker work and de-advertise unfinished UI.

Two HEAD features are implemented but unsafe to advertise:

1. Epoch-18 scheduling can persist a claimed occurrence, thread, run, and
   binding in separate operations. A crash can therefore leave an unbound
   partial run and create a duplicate on recovery. It also queues work without
   a qualified isolated consumer.
2. Epoch-19 Wisp lifecycle verification may verify fixture-provided signing
   bytes without proving they encode the manifest fields later trusted, and
   authoritative lifecycle state is stored as non-transactional JSON.

The dirty ACP work includes credential-file copying, recursive runtime residue
deletion, and host-dependent tests. Those changes conflict with credential and
filesystem boundaries and must be split or removed before any commit.

The checked-in implementation-status and protocol chronicle still report IPC
epoch 16 even though source accepts epoch 19. Correcting that documentation is
part of the first forward-fix milestone.

## Implementation progress after the audit

The unsafe epoch-18 scheduler and epoch-19 Wisp mutations are now unavailable
in epoch 20, while their read projections remain fail-closed. Unsafe ACP
credential import and recursive cleanup work was removed. Tray behavior,
close-to-notification-area settings, and renderer capability honesty have
focused regression coverage.

Epoch 21 adds a daemon-owned SuperGrok API Chat rail using only fresh official
xAI OAuth device authorization with `api:access`. Tokens remain in the daemon
vault, refresh/disconnect are serialized against provider initiation, and
cancellation cannot commit a late grant. The standard `api.x.ai` Responses
adapter uses bearer authorization only. New turns persist either the
`xai_api_key` or `supergrok_api` rail and retries/forks cannot switch it.
Successful enrollment selects SuperGrok for new conversations at runtime;
disconnect selects the user-owned API-key rail. Grok Build ACP authentication
and usage remain a separate product rail.

The completion pass now adds the locally implementable portions of phases 2–4:
schema 21 atomically claims and binds each scheduled occurrence to its dedicated
thread, immutable prompt, queued run, and dispatch identity; the daemon can
resume only exact bound queued work and atomically marks ambiguous in-flight
work `interrupted_needs_review`. The closed Linux `scheduled.run` contract
carries no workspace, tools, shell, MCP, host path, or credential authority and
remains unavailable without signed broker/guest qualification and a daemon-
owned proof.

Schema 22 replaces Wisp's JSON lifecycle authority with a SQLCipher journal.
Canonical parsed manifests, a complete signed file inventory, independent
catalog trust, retained file identity, private staging, exact idempotency, and
acknowledged-only projection now cover install, update, recovery, and rollback.
IPC mutations remain unavailable until production release roots and platform
publication qualification exist. These are intentional external gates, not
mockable completion work.

The Electron bridge and Setup view expose only bounded non-secret status,
verification URI, and user code. Wisp headless verified the Setup landmarks,
device-code presentation, and cancel transition in an isolated Chromium
session. Focused evidence at this tip includes 516 renderer tests, 33 desktop
script tests, tray asset validation, desktop TypeScript checks, 50 application
tests, 30 xAI adapter tests, 27 protocol tests, and 49 daemon tests.

The public-release completion pass now also includes a signed, manifest-gated
Windows updater and a digest-verified Linux AppImage updater with rollback,
plus a protected tag release workflow. Distribution remains externally gated
on production signing identities, update trust roots, and qualified Windows
workers. Home Chat has a daemon-owned product prompt that states its actual
capabilities, and isolated headless Electron CDP QA covers every route at wide
and narrow viewports without sharing the user's durable daemon database.

The next locally implementable daemon capability is request-scoped official
xAI Search. It requires an epoch and schema change because the selected closed
server-tool set must be persisted on each turn and preserved by retry, edit,
regenerate, and restart recovery. The existing provider-request fingerprint
already binds continuation and ordered server tools as a prerequisite.
Research is not Search: it remains unavailable until it has its own bounded,
durable orchestration contract.

## Reading order

1. [Review findings](01-review-findings.md)
2. [Implementation roadmap](02-implementation-roadmap.md)
3. [Testing and qualification](03-testing-and-qualification.md)
4. [Agent handoff](04-agent-handoff.md)
5. [Commit plan](05-commit-plan.md)
6. [Open risks and external gates](06-open-risks-and-external-gates.md)
7. [SuperGrok API Chat research](../research/supergrok-api-chat-contract.md)

`ai-continue-plan.md` and `ui-plan.md` are historical working notes, not
canonical status sources.
