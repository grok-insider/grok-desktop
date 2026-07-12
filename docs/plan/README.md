# Audited recovery and completion plan

- Snapshot: 2026-07-12
- Reviewed HEAD: `66787d5ef3fe4bd23aadb49951b8d347ab32e8b6`
- Review range: `226484610d5ea4ecbb1c614cc7a741a2d054fc65..HEAD`
- Wire/schema observed in source: IPC epoch 19, SQLCipher schema 19
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
