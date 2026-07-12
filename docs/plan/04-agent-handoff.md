# Agent handoff contract

## Ownership

One integration lead owns protocol epochs, schema numbering, generated output,
daemon composition, canonical status docs, and final gates. Delegate bounded,
non-overlapping work packages:

| Package | Primary ownership | Must not edit concurrently |
|---|---|---|
| Scheduler | domain/application plus memory/SQL adapters | protocol and daemon composition without lead coordination |
| Wisp lifecycle | integration service plus its persistence adapter | shared migration registry or protocol without lead coordination |
| ACP/Linux | ACP adapter, packaging, Linux broker/transport | Chat credentials or Wisp state |
| UI/QA | renderer views/tests and Wisp evidence | daemon policy, generated protocol, durable state |
| SuperGrok OAuth | OAuth adapter, vault service, contract tests | scheduler, Wisp, ACP runtime |

## Merge order

1. Documentation and dirty-work classification.
2. Fail-closed protocol correction.
3. ACP/Linux corrections.
4. Atomic scheduler implementation.
5. Durable Wisp lifecycle.
6. Product regression and release evidence.
7. SuperGrok implementation after ADR 0026 and the invariant amendment land.

Agents re-read `AGENTS.md` and the relevant architecture/ADR docs before
editing. Shared protocol, migration, and composition-root changes are serialized.

## Required handoff report

Every agent reports:

- base and resulting commits;
- files changed and why;
- product/security invariants checked;
- tests run with exact results;
- generated-file and migration status;
- skipped/external gates and unresolved risks;
- confirmation that credentials, account identifiers, private paths, raw
  provider responses, and untrusted payloads were not logged or committed.

Do not conceal partial failures behind aggregate commands. Preserve unrelated
dirty work, do not rewrite history, and do not push.
