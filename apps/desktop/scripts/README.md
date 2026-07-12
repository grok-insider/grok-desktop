# Local Electron CDP harness

Canonical debugging and QA procedures (including Wisp/Hyprland and when not to
touch the host desktop):
[docs/development/debugging-and-qa.md](../../../docs/development/debugging-and-qa.md).
This file is the CDP launcher and quality-probe detail.

From the repository root, build the locked Rust daemon (with the development
ACP descriptor feature so Setup can connect Grok Build plans) and deterministic
Electron assets, then launch a named persistent QA profile:

```sh
pnpm build:dev-daemon
pnpm dev:cdp -- --profile qa-local --port 9250
```

Unpackaged launches auto-detect the official `grok` CLI on `PATH` (or honor
explicit `GROK_ACP_EXECUTABLE` / `GROK_ACP_VERSION` / `GROK_ACP_SHA256`) and
forward them into the daemon child. Packaged launches never receive those
overrides; product packages need a release daemon with catalog trust keys plus
an optional staged `components/grok-acp` layout (see `docs/platform/linux-release.md`).

The launcher preflights `127.0.0.1:9250`, stores Electron user data under the
operating system's local state directory, and records the exact launcher and
Electron PIDs inside that profile while the session is running. It never scans
for or terminates unrelated Electron or daemon processes. Stop it with
`Ctrl+C`; the Electron main process then performs its normal daemon shutdown.

An absolute `GROK_PINENTRY` override is forwarded only by an unpackaged
development launch; packaged launches strip it, and the daemon revalidates the
canonical executable before use. Without an override, Unix lookup accepts only
a protected root-owned canonical `pinentry` or `pinentry-*` target. The launcher
sends the daemon's 32-byte IPC bearer nonce over its one-shot stdin pipe. The
legacy `GROK_DAEMON_STARTUP_NONCE_HEX` environment handoff is intentionally
rejected and must not be restored as a compatibility switch.

In another terminal, run the non-interactive production quality probe:

```sh
pnpm test:e2e:electron -- --port 9250
```

The probe attaches to the already-running renderer and checks the production
protocol, isolated preload bridge, strict CSP, read-only route rendering,
visible error alerts, uncaught exceptions/`console.error`, semantic landmarks,
accessible names, desktop and narrow horizontal overflow, the
`prefers-reduced-motion` override, documented semantic text/status contrast
pairs, and the actual opaque focus outline on canvas and card surfaces. The
contrast probe resolves only the explicit `DESIGN.md` token allowlist; it does
not use a noisy all-element heuristic. It temporarily visits Home, Projects,
Activity, Library, Automations, Extensions, Settings, and Setup, then restores
the original hash. Every route receives the accessibility, responsive, and
reduced-motion probes. Settings waits for credential status and Setup waits for
its daemon-backed busy region to settle before the probe continues. Viewport and
media emulation are cleared in a `finally` block.

The route set intentionally excludes Conversations because they require a
fixture-backed durable thread. The probe never clicks controls, enters text,
reads or enrolls credentials, or starts provider/tool work. It does reset
transient in-memory state within the visited read-only views, so run it against
the dedicated QA profile rather than a working session with an unfinished
dialog. A passing run covers deterministic structure, responsiveness, and the
semantic contrast contract; it does not replace keyboard, screen-reader,
visual-regression, content-specific contrast, or Windows qualification testing.

For exploratory Chrome DevTools MCP testing, point a dedicated MCP process at
the same endpoint (keep it separate from any MCP instance attached to a normal
browser profile):

```sh
NO_PROXY=127.0.0.1,localhost npx chrome-devtools-mcp@latest \
  --browserUrl http://127.0.0.1:9250
```

The browser-only renderer preview is a separate, explicit surface:

```sh
pnpm dev:web
```

Opening the renderer without either Electron's preload bridge or that explicit
preview command fails closed instead of loading interactive sample data.

## Hyprland GUI verification

For visual GUI verification on this development host, reserve Hyprland
workspace **3** for Grok Desktop, ensure no other application is present there,
and maximize or fullscreen the Grok Desktop window before capturing or judging
the UI. Apply that workspace setup through the external Wisp/Hyprland QA flow;
the launcher and quality probe intentionally do not switch workspaces, resize
native windows, or rearrange a developer's desktop.
