# Wisp managed adapter

Wisp is the recommended computer-use add-on, but it is not part of the desktop
process and does not share the desktop lifecycle. The guest integration runner
starts, health-checks, restarts, updates, and stops it as an independent managed
unit.

The adapter launches `wisp-mcp` directly (never through a shell) and exposes only
the lifecycle and computer-use methods declared in `adapter.json`. Wisp CLI, TUI,
device inspection, guest execution, real-desktop, autonomous-provider, and remote
control methods are not routed through this adapter.

For each observation, the adapter:

1. Captures the nested Wayland surface and allocates a strictly increasing
   `revision` for that application instance.
2. Emits the stable application ID and the current runtime instance ID with the
   observation.
3. Accepts an action only when both IDs and `observationRevision` still match the
   current surface.
4. Clamps the action to the v1 schema limits, then maps it to the corresponding
   Wisp pointer, keyboard, text, scroll, or wait operation.
5. Returns `stale-observation` or `wrong-application` without acting when the
   precondition fails.

`stdout` is reserved for UTF-8 JSON Lines protocol messages. Diagnostics go to
`stderr`; secrets, user-entered text, and screenshots must not be logged.

The checked-in manifest uses the `development` update channel and is deliberately
unsigned. It is source material, not an installable release bundle. Packaging must
canonicalize and sign the final manifest with Ed25519 before changing the update
channel to `preview`, `nightly`, or `stable`.
