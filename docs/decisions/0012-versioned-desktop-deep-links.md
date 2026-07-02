# ADR 0012: Versioned desktop deep-link activation

- Status: Accepted
- Date: 2026-07-11

## Context

Grok Desktop needs operating-system activation links that restore the existing
single-instance window and navigate to an authorized local view. The
`grok-desktop` scheme already serves packaged renderer assets under the private
`app` authority, so treating arbitrary scheme URLs as renderer locations would
mix an untrusted public input with the application's internal origin. Deep links
must not carry prompts, commands, file paths, external URLs, credentials, or
provider input, and they must not bypass the daemon's ownership of entities.

Cold launch arguments, second-instance arguments, and macOS `open-url` events
are untrusted process-boundary input. Renderer readiness also races cold-start
delivery, while a tray-hidden primary window must be restored before navigation
is visible.

## Decision

The public activation contract is a closed, versioned v1 grammar under the
separate `grok-desktop://open/` authority:

- `grok-desktop://open/v1/{home|projects|activity|library|automations|extensions|settings}`
- `grok-desktop://open/v1/projects/project-<bounded ASCII ID>`
- `grok-desktop://open/v1/conversations/thread-<bounded ASCII ID>`

Electron main is the sole parser. It rejects the internal `app` authority,
unknown versions and routes, non-canonical URL forms, user information, ports,
queries, fragments, controls, percent encoding, traversal, unsafe or cross-kind
identifiers, and overlong input. Process argument lists must contain exactly one
valid activation link; multiple valid links fail closed.

- Main sends only the closed structured route union through a dedicated preload
  event. The raw URL never enters the renderer.
- The preload installs the typed listener before sending a readiness handshake.
  Main validates that handshake against the primary window's main frame and
  private application origin.
- At most one pending route is retained; a newer validated activation replaces
  an older unacknowledged one. Main assigns a bounded delivery ID and retains
  the latest route until the trusted preload acknowledges that the renderer
  listener accepted it. A full-document reload makes the renderer unavailable
  until a fresh handshake and then redelivers; HashRouter same-document changes
  do not invalidate readiness.
- Second-instance and `open-url` activation restores, shows, and focuses the
  primary window before delivering a route.
- MSIX registers exactly one lowercase `grok-desktop` protocol extension bound
  to the packaged full-trust Grok Desktop executable and passes one quoted `%1`
  URI argument. Actual installation and URI activation remain Windows
  qualification evidence.

## Consequences

- Public links can select only existing views or opaque daemon-owned entity IDs;
  they cannot execute work, submit chat content, open local files, or launch an
  external URL.
- The internal renderer origin remains non-navigable from public activation
  input even though it shares the scheme.
- Unknown future link versions are ignored until a new parser and compatibility
  decision are shipped.
- An entity may have been deleted or be unavailable locally; the existing view
  then presents its normal unavailable or canonical fallback state without
  granting additional lookup authority to the link.
