# Official Grok integration surfaces

- Research date: 2026-07-10
- Product rule: every model response comes from an official Grok/SpaceXAI service

## Supported trust paths

### Grok subscription through official Grok Build

xAI documents Grok Build as an interactive agent, a headless command, and an
ACP agent for embedding in other applications. Its first-party login paths are
browser OIDC and device code. Grok Desktop delegates subscription
authentication and refresh to that official component; it does not implement a
private Grok web OAuth client, accept browser cookies, or call web application
endpoints.

Source: [Grok Build overview](https://docs.x.ai/build/overview)

The desktop speaks the versioned Agent Client Protocol using the published ACP
SDK. The official component owns its provider session. Grok Desktop owns local
session mapping, approval policy, persistence, cancellation, process
supervision, and the execution boundary.

### User-owned xAI API key

xAI's public API uses bearer API keys at the official `api.x.ai` service. Grok
Desktop stores a user-provided key in the operating-system credential vault and
uses it only from the Rust daemon. The base service is compiled into the
adapter; there is no configurable compatible endpoint.

Source: [xAI API quickstart](https://docs.x.ai/developers/quickstart)

The direct adapter discovers the models available to the key rather than
embedding a permanent model catalog. Supported product surfaces are enabled
only when the live API and selected model advertise the needed capability.
Current official surfaces include Responses, image and video generation,
Files/Collections, hosted search tools, and realtime voice.

Sources:

- [Inference REST API](https://docs.x.ai/developers/rest-api-reference/inference/chat)
- [xAI gRPC API and model discovery](https://docs.x.ai/developers/grpc-api-reference)
- [Voice API](https://docs.x.ai/developers/rest-api-reference/inference/voice)

An xAI API key is a separate credential and billing path. Configuring one does
not convert subscription entitlement into API credit, and removing it does not
sign the user out of Grok Build.

## SuperGrok plans and API entitlement

The current SpaceXAI pricing surface lists Free, SuperGrok Lite, SuperGrok,
SuperGrok Heavy, Business, and Enterprise product plans. The Grok product guide
describes paid SuperGrok usage as a shared weekly allowance across Grok product
features. The developer API instead requires an xAI API account with separately
funded credits and a team-bound API key. A desktop sign-in or paid plan must
therefore never be treated as API billing authority, and a configured API key
must never be presented as a SuperGrok subscription.

Sources:

- [Compare Grok plans](https://x.ai/pricing)
- [Grok plans and usage](https://docs.x.ai/grok/overview)
- [xAI API quickstart](https://docs.x.ai/developers/quickstart)

Plan names are display metadata, not capability switches. The desktop derives
subscription behavior from the authenticated official ACP session and its live
capabilities. It derives BYOK behavior from model discovery and provider
responses. This avoids embedding a plan matrix that can change independently
of the application.

API keys can be team-bound, model- and endpoint-scoped, disabled, expired, and
rate-limited. A successful vault write alone is not readiness; validation and
each operation must preserve distinct unauthorized, forbidden/scope, rate,
transport, and provider failure states. The product must also disclose the
provider data policy applicable to the account. SpaceXAI currently documents
temporary API request/response storage by default and an Enterprise-only Zero
Data Retention option whose state is reflected in an API response header.

Sources:

- [API key authorization and ACLs](https://docs.x.ai/developers/rest-api-reference/management/auth)
- [xAI API security and retention](https://docs.x.ai/developers/faq/security)

## Mandatory Grok-only controls

Grok Build intentionally supports custom models, arbitrary base URLs, local
MCP servers, plugins, hooks, and compatibility imports from other tools. Those
are useful in the standalone CLI but cannot be inherited by this Grok-only
application.

Sources:

- [Grok Build settings and `GROK_HOME`](https://docs.x.ai/build/settings)
- [Grok Build enterprise requirements](https://docs.x.ai/build/enterprise)
- [Grok Build MCP discovery](https://docs.x.ai/build/features/mcp-servers)

The managed ACP runtime therefore must:

1. Set `GROK_HOME` to a dedicated application-owned directory and never read
   the user's normal `~/.grok` tree.
2. Generate a pinned `requirements.toml` that disables API-key auth for the ACP
   path and requests the bypass-permission lock. BYOK remains in the separate
   direct xAI adapter. SpaceXAI documents that the bypass lock is authoritative
   only from a root-owned system requirements file, so a host user-level file
   is not treated as a security boundary.
3. Start with a cleared environment and never pass `XAI_API_KEY`, arbitrary
   provider variables, plugin secrets, or inherited Node/Python injection
   variables.
4. Disable compatibility imports and unmanaged MCP/plugin/hook discovery. A
   managed integration is launched by the guest supervisor only after signed
   manifest verification and explicit grants.
5. Keep the ordinary `HostControl` ACP role authentication/control-only. An
   independently risk-enrolled Host Tools run may use the separate constrained
   `HostWorkTools` role with only the daemon-owned authenticated loopback HTTP MCP bridge;
   it does not inherit arbitrary native tools, MCP discovery, plugins, hooks,
   or Chat/scheduler authority. Isolated Work still executes inside the
   qualified guest. Backend failure never switches roles or creates a grant.
6. Verify the component against a signed release catalog and revalidate its
   file identity immediately before spawn. A filename, caller-provided
   publisher string, or caller-provided digest alone is not provenance.

## Capability routing

Capability routing is explicit rather than a generic provider abstraction:

| Capability | Subscription ACP | xAI BYOK | Local platform |
| --- | --- | --- | --- |
| Grok agent session | Yes | No | Session mapping only |
| Text/image conversation | When ACP advertises it | When model advertises it | Durable thread and artifact state |
| Web/X research | When official runtime advertises it | Official hosted tools | Policy and citation storage |
| Images/video | No assumed subscription contract | Official Imagine APIs | Download, provenance, library |
| Realtime voice | No assumed subscription contract | Official ephemeral-token flow | Audio device broker |
| Files/shell/local MCP | Isolated Guest ACP, or constrained HostWorkTools after explicit enrollment | Never ambient | VM for isolation; otherwise daemon roots, exact approvals, effect journal |
| Browser/computer use | Managed tool contract only | Never ambient | Broker plus stale-frame checks |

No capability is inferred from a plan name. The daemon combines live
authentication, runtime negotiation, model discovery, platform readiness, and
enterprise policy into the status shown by the desktop.
