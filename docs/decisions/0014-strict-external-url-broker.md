# ADR 0014: Strict external-URL broker

- Status: Accepted
- Date: 2026-07-11

## Context

Official xAI responses may include citation metadata, but provider output is
untrusted. Rendering a citation as an ordinary anchor, calling `window.open`,
or exposing Electron shell methods through preload would let renderer content
choose a privileged navigation target. The existing navigation policy correctly
denies popup creation and prevents the application window from leaving its
private document; source opening must not weaken either control.

Opening a source in the user's operating-system browser is a desktop-shell
action, not provider execution or durable domain state. It therefore belongs to
Electron main, while the renderer may express only an explicit user request to
open one displayed citation. This contract must remain separate from the public
`grok-desktop://open/` activation grammar in ADR 0012.

## Decision

The isolated preload exposes one tagged request through the existing typed
bridge: `desktop.openExternalUrl { url }`. It does not expose Electron `shell`,
a generic navigation primitive, a disposition, headers, a local path, or a
custom scheme. Citation chips continue to open the local source inspector; only
the inspector's explicit **Open source in browser** button submits the request.
No renderer anchor receives the untrusted source URL.

Electron main applies all authority and validation at the moment of use:

- The sender must be the primary window, its sender frame must be the top-level
  main frame, and the frame URL must still be the exact private application
  document (with only hash routing permitted).
- The request must contain exactly `kind` and `url`. The URL is bounded to 8,192
  UTF-8 bytes and must equal its WHATWG canonical serialization: lowercase
  HTTPS scheme and DNS host, an explicit root slash, no default or custom port,
  no dot-segment normalization, and canonical percent escapes.
- User information, raw controls or whitespace, non-HTTPS/file/custom schemes,
  all IPv4 and IPv6 literals (including legacy numeric spellings), single-label
  names, IDNs/punycode, and local/private special-use suffixes are rejected.
  Rejecting every IP literal is deliberately stricter than enumerating only
  private and link-local ranges. A DNS name is not treated as proof of a fixed
  network address; this broker makes no DNS-rebinding claim.
- At most one operating-system launch is in flight and at most four launches
  are admitted in a rolling ten-second window. This limits a compromised
  renderer because generic preload IPC cannot prove a physical user gesture.
- Only the validated canonical string reaches `shell.openExternal`. Validation,
  rate-limit, and operating-system failures return closed, fixed result codes;
  native error details never enter renderer state and the action is never
  retried automatically.

The existing `setWindowOpenHandler` denial, `will-navigate` policy, renderer
sandbox, context isolation, CSP, and permission denial remain unchanged.

## Consequences

- Users can open a displayed public HTTPS citation in their default browser
  without granting the renderer general shell or navigation authority.
- A citation may remain visible in the inspector yet be refused by the stricter
  desktop boundary. The UI reports that refusal without navigating or retrying.
- Local files, intranet names, localhost, IP literals, alternate schemes, and
  ambiguous/noncanonical URL spellings cannot reach the operating-system shell.
- Browser-preview fixtures cannot launch external sources; they return an
  explicit unavailable result because no native broker exists.
