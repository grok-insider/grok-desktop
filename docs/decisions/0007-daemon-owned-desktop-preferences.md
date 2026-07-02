# ADR 0007: Daemon-owned desktop behavior preferences

- Status: Accepted
- Date: 2026-07-11

## Context

Window-close behavior must survive restarts and be shared consistently by the
renderer and Electron main process. Renderer storage and Electron-local files
would create a second durable authority outside the Rust daemon.

## Decision

Protocol epoch 3 added `GetDesktopPreferences` and
`UpdateDesktopPreferences`; current epoch 14 retains them unchanged. The initial revisioned setting is
`keep_running_in_notification_area`, which defaults to `true`. Updates require
an observed revision and an idempotency key; the encrypted store commits the
new singleton snapshot and exact replay result in one transaction.

Electron main may cache the latest daemon response only for immediate window
lifecycle decisions. It does not persist that cache. A normal close hides the
window when the setting is enabled. Disabling it restores normal close-to-quit
behavior. The tray's explicit Quit action always bypasses hiding and completes
the existing graceful daemon shutdown path.

## Consequences

- Settings remain usable across Windows and Linux without renderer-owned state.
- Ambiguous transport failures can safely retry the same update.
- An unavailable daemon leaves the product default active and exposes the
  preference as unavailable rather than inventing another persistence path.
- Future desktop preferences can extend the same revisioned message; breaking
  semantics still require a new compatibility epoch.
