# Debugging and QA

Procedures for verifying Grok Desktop locally. Prefer the lightest surface that
answers the question. When the user is active, launch or move the app into
Wisp's hidden compositor and attach through Chrome DevTools MCP/CDP. Do **not**
rearrange, switch, or take focus on the user's visible desktop.

Harness detail for the CDP launcher lives in
[apps/desktop/scripts/README.md](../../apps/desktop/scripts/README.md).

## Choose a surface

| Goal | Surface | Commands / tools |
|------|---------|------------------|
| Renderer layout, routes, sample UI without daemon | Browser preview | `pnpm dev:web` + headless browser or DevTools |
| Production Electron shell, CSP, preload, daemon | CDP QA profile | `pnpm dev:cdp` + `pnpm test:e2e:electron` |
| Semantic DOM / a11y while Electron runs | Chrome DevTools Protocol | attach to the CDP port (default **9250**) |
| Native window, tray, focus, fullscreen screenshots | Wisp hidden compositor + CDP | hidden compositor by default; visible workspace **3** only when explicitly released |
| Unit / integration without UI | Tests | `pnpm test`, `cargo test --workspace`, Go package tests |

## Renderer-only preview

```sh
pnpm dev:web
```

Uses the explicit browser preview path with sample data. Suitable for visual
and route checks when the daemon bridge is not required.

**Wisp (developer tooling):** when available on the host, Wisp’s independent
headless browser can inspect this preview without moving host workspaces. Close
the exact Wisp/Vite owners when finished so ports do not leak.

This is **not** the product Wisp integration. Product computer-use is the
managed adapter described in
[integrations/first-party/wisp/ADAPTER.md](../../integrations/first-party/wisp/ADAPTER.md).

## Production Electron + CDP

Terminal 1 — build and launch a named persistent QA profile:

```sh
pnpm dev:cdp -- --profile qa-local --port 9250
```

Notes from the harness:

- Preflights `127.0.0.1:9250`.
- Stores Electron user data under the OS local state directory for that profile.
- Records launcher and Electron PIDs in the profile; does not scan or kill
  unrelated processes.
- Stop with `Ctrl+C` for normal daemon shutdown.
- Use a dedicated QA profile; the e2e probe resets transient in-memory state in
  visited read-only views.

Terminal 2 — non-interactive quality probe:

```sh
pnpm test:e2e:electron -- --port 9250
```

The probe attaches to the running renderer and checks production protocol,
isolated preload bridge, strict CSP, read-only route rendering, visible error
alerts, uncaught exceptions / `console.error`, landmarks, accessible names,
overflow, `prefers-reduced-motion`, documented semantic contrast pairs, and
focus outlines. It visits Home, Projects, Activity, Library, Automations,
Extensions, Settings, and Setup, then restores the original hash.

**Intentionally excluded:** Conversations (need fixture-backed durable threads).
**Never does:** click controls, enter text, enroll credentials, or start
provider/tool work.

A pass covers deterministic structure, responsiveness, and the semantic contrast
contract. It does **not** replace keyboard, screen-reader, visual-regression,
content-specific contrast, or Windows qualification testing.

### Chrome DevTools MCP

For exploratory inspection, point a dedicated MCP process at the same endpoint
(keep it separate from any MCP instance on a normal browser profile):

```sh
NO_PROXY=127.0.0.1,localhost npx chrome-devtools-mcp@latest \
  --browserUrl http://127.0.0.1:9250
```

On hosts where port 9222 is occupied, keep **9250** unless both launcher and
probe are changed together.

## Native GUI without focus stealing

The default workflow while the machine is in use is:

1. Open Grok Desktop inside Wisp's hidden compositor, or move its exact window
   there before interaction.
2. Launch with a dedicated CDP profile and port (normally `qa-local`, 9250).
3. Connect Chrome DevTools MCP/CDP to that Electron renderer for DOM, console,
   network, accessibility, and state inspection.
4. Use Wisp only for native window, tray, focus, sizing, input, and screenshots.
5. Close the exact Wisp, Electron, Vite, and CDP-profile owners when finished.

If the hidden compositor cannot start, treat that as a tooling blocker for
native visual QA. Browser-preview/headless and CDP semantic checks may continue,
but do not substitute a visible workspace without the user's permission.

## Visible native GUI on Hyprland (only when released)

For visual GUI verification of the real Electron window:

1. Reserve Hyprland workspace **3** for Grok Desktop alone.
2. Confirm the workspace is empty (`hyprctl -j clients` or Wisp).
3. Launch `pnpm dev:cdp -- --profile qa-local --port 9250` and keep it attached.
4. Move/focus Grok Desktop on workspace 3; maximize or fullscreen so layout is
   judged at full available size.
5. Run `pnpm test:e2e:electron -- --port 9250` from another terminal if needed.
6. Use CDP / DevTools for semantic renderer assertions; use Wisp/Hyprland for
   native windows, tray, focus, and full-screen screenshots.

The launcher and quality probe **intentionally do not** switch workspaces,
resize native windows, or rearrange a developer’s desktop. Apply workspace
setup through the external Wisp/Hyprland flow (or manually).

### Courtesy rule

If the user is using the computer, do not switch workspaces or move windows on
the visible compositor. Use Wisp's hidden compositor plus CDP. If it is
unavailable, prefer `pnpm dev:web` + headless browser QA and defer native checks
until the desktop is explicitly released.

## Two meanings of “Wisp”

| Meaning | What it is | Doc |
|---------|------------|-----|
| **Product integration** | Managed, out-of-process computer-use add-on (`wisp-mcp`), optional, signed manifests | [ADAPTER.md](../../integrations/first-party/wisp/ADAPTER.md) |
| **Developer QA tooling** | Host skill/MCP to drive GUI, browser, TUI for verification | This page + host Wisp docs (e.g. skill `wisp-debugging`); not a repo runtime dependency |

Never compile developer-host Wisp into the desktop binary or treat product Wisp
as required for Chat or core UI.

## Windows and release QA

- Packaging and fuses: [windows-release.md](../platform/windows-release.md)
- Full release matrix: [release-qualification.md](../quality/release-qualification.md)
- Implementation ledger: [implementation-status.md](../quality/implementation-status.md)

HCS and packaged-service tests belong on documented Windows qualification
workers, not as optional host fallbacks on Linux.

## Related

- [Local development](local-development.md)
- [Coding guidelines](coding-guidelines.md)
- [AGENTS.md](../../AGENTS.md)
