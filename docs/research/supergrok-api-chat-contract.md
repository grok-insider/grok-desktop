# SuperGrok usage split and API Chat contract research

- Research date: 2026-07-12
- Local evidence repository: `~/dev/opensource/open-usage`
- Inspected open-usage commit: `6764adc6fa729337d15bab90c9d8e8e28b28646c`
- Upstream OpenCode evidence: commit `34e58090595d44e3e7cc37498f16753a98627456`
- Status: product split verified; repository owner authorized the fixed OAuth/API contract

## What the local evidence proves

`open-usage/src/providers/grok.rs` queries the official Grok CLI billing origin
and parses `creditUsagePercent` as a shared weekly pool. It maps official
`productUsage[].product` values as follows:

| Wire value | Display label |
|---|---|
| `GrokBuild` | Build |
| `Api` | Api |
| `GrokChat` | Chat |

This matches the observed account display: one weekly percentage with separate
Build, API, and Chat percentages. `open-usage` also uses the official Grok CLI
credential file and special CLI authentication header to read billing/settings.
That makes it useful evidence of product accounting, but not an implementation
to copy into Grok Desktop.

## What it does not prove

- It does not prove that Build, API, and Chat are interchangeable endpoints.
- It does not prove that a SuperGrok token may be sent to `api.x.ai` by Grok
  Desktop or that the public client ID is approved for this application.
- It does not prove that an API response consumes `Api` rather than `GrokChat`.
- It does not authorize importing `~/.grok/auth.json`, browser cookies, refresh
  tokens, or CLI-specific authentication headers.
- User authorization to work on this repository does not itself establish an
  official provider contract.

## Approved repository decision

ADR 0026 permits a distinct daemon-owned `SuperGrokApi` rail. It uses a fresh
OAuth grant, the public xAI desktop/CLI client, fixed `auth.x.ai` endpoints,
the approved `api:access` scope, and normal `api.x.ai` Responses traffic. It
does not import another client's tokens, use the CLI chat proxy, or change Grok
Build ACP ownership.

## Contract locked for implementation

The implementation must retain:

1. the public client ID and fixed authorization/token/device endpoints;
2. authorization-code PKCE plus RFC 8628 device flow;
3. exact approved scopes and `referrer=grok-desktop` attribution;
4. vault-only rotating tokens and fixed `api.x.ai` provider origin;
5. immutable per-turn rail lineage and no silent fallback;
6. no tools/workspace authority for Home Chat;
7. Rust provider and durable-turn authority.

## Qualification if approved

With explicit user approval for a bounded real-account test:

1. record only redacted pre-request Build/API/Chat counters;
2. enroll through the approved flow without reading CLI credentials;
3. discover models and perform one bounded text-only request;
4. verify durable streaming without an API key or Work/VM dependency;
5. wait for accounting and record only which product changed;
6. revoke/disconnect and prove fail-closed behavior;
7. ensure no token, JWT claim, account ID, raw billing response, or precise
   private usage value enters logs, fixtures, screenshots, or commits.
