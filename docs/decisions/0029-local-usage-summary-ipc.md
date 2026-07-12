# ADR 0029: Local usage summary IPC

- Status: Accepted
- Date: 2026-07-12

## Context

Grok Desktop already persists official Responses `usage` on each completed
conversation turn. Product surfaces need aggregates by chat, project, and
rolling windows (7 / 30 days) without routing traffic through third-party
tools (open-usage) or inventing tokens.

## Decision

Protocol epoch 23 adds read-only `GetUsageSummary`:

- Scope: `workspace` | `project` | `thread`
- Window: `last_7_days` | `last_30_days` | `all_time`
- Source: completed turns only (`conversation_turns` SQL aggregate)
- Authority: daemon SQLCipher; renderer cannot recompute from partial history

No provider network access. No secrets. Aggregates never enter model prompts,
crash reports, or broad logs.

Subscription SuperGrok weekly allowance remains a separate future probe under
daemon-owned SuperGrok OAuth credentials (not this operation).

## Consequences

- Settings, conversation headers, and project views can show honest Desktop
  usage without open-usage.
- Missing project/thread IDs fail closed as not found.
- Cost display is optional when `cost_in_usd_ticks` is zero.

## Rejected alternatives

- Renderer-side summation of loaded messages
- open-usage reverse-proxy capture as a required path
- Inventing allowance percentages from local token totals
