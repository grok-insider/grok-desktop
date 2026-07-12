# ADR 0031: Durable official xAI Search grants

## Context

The xAI Responses adapter already supports the official `web_search` and
`x_search` server tools, but Home Chat could not request them. A renderer-only
toggle would be unsafe: retries, forks, and restart recovery could reconstruct a
different provider request, and an idempotency replay could silently change the
billable tool set.

## Decision

Protocol epoch 24 adds `search_enabled` to conversation-start intent and to the
turn projection. SQLCipher schema 24 adds a non-null checked column with a
fail-safe `false` backfill for older turns. The domain aggregate owns this
immutable bit before dispatch. Command and provider-request fingerprints bind
it, and derived turns inherit it from their canonical source.

`true` is a closed product preset that maps only to xAI's official
`web_search` and `x_search` server tools. It does not accept arbitrary tool
names and grants no filesystem, shell, MCP, browser-control, workspace, Work,
or host authority. Research remains a separate unavailable capability.

## Consequences

- Exact replay conflicts if the caller changes Search under one command key.
- Retry, edit-and-branch, regenerate, and recovery preserve the source grant.
- Existing turns migrate with Search disabled and cannot acquire it later.
- The renderer can honestly display which turns were search-enabled without
  becoming the durable authority.
