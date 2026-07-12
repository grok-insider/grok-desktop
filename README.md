# Grok Desktop

Grok Desktop is a Windows-first, local-first desktop workspace designed for
official Grok and SpaceXAI services through documented Grok Build and xAI API
contracts. The architecture covers conversational work, durable projects, agent
tasks, files and artifacts, browser and computer automation, media generation,
voice, integrations, and local schedules behind explicit security boundaries.

> Grok Desktop is an independent project and is not affiliated with, endorsed
> by, or sponsored by SpaceXAI. Grok, SpaceXAI, and xAI are trademarks of their
> respective owners. The application uses a custom project identity and
> official service contracts only.

## Authentication

- **Subscription:** OAuth and model sessions via the official Grok Build client
  (Agent Client Protocol).
- **BYOK:** a user-owned xAI API key for documented direct APIs. It is not
  SuperGrok subscription credit and never enables another provider.

The Electron renderer is not a credential-entry boundary. Keys stay outside
renderer, preload, Electron-main, argv, and environment state (native
enrollment on Windows; bounded pinentry on Linux). See
[ADR 0005](docs/decisions/0005-native-credential-enrollment.md).

Direct Chat, forks, artifacts, search, and related contracts are daemon-owned
and versioned over local Protobuf IPC. Current surface and schema chronicle:
[docs/architecture/protocol-and-persistence.md](docs/architecture/protocol-and-persistence.md).

No third-party model provider, arbitrary compatible endpoint, private Grok web
API, or imported browser cookie is supported. Official surface research:
[docs/research/official-grok-surfaces.md](docs/research/official-grok-surfaces.md).

## Architecture

The Electron/React application is a sandboxed presentation shell. A separate
Rust daemon is the system of record for domain rules, encrypted persistence,
provider integrations, approvals, scheduling, MCP, and worker orchestration.
Strong Work uses a brokered Linux utility VM behind a narrow Windows service;
builds fail closed into Limited Mode until that path is qualified.

| Doc | Contents |
|-----|----------|
| [docs/README.md](docs/README.md) | Documentation map |
| [docs/architecture/overview.md](docs/architecture/overview.md) | Process model and authority |
| [docs/architecture/principles.md](docs/architecture/principles.md) | Clean Architecture, SOLID, clean code |
| [docs/architecture/modules.md](docs/architecture/modules.md) | Module ownership |
| [AGENTS.md](AGENTS.md) | Agent checklist (invariants, commands) |

## Development

There are no packaged releases yet; run from source.

**Prerequisites:** Node.js >= 22.22 (corepack; `pnpm@10.33.2`), Rust 1.95+,
Go (Windows service / guest runner), Nix optional.

```sh
pnpm install --frozen-lockfile

pnpm dev        # Vite + Electron
pnpm dev:web    # browser-only renderer preview
pnpm dev:cdp    # QA profile with CDP (port 9250 typical)

pnpm check      # lint + typecheck + test + build + Rust gates
```

Full bootstrap, gates, protocol regen, debugging (CDP, e2e, Wisp/Hyprland):

- [docs/development/local-development.md](docs/development/local-development.md)
- [docs/development/debugging-and-qa.md](docs/development/debugging-and-qa.md)
- [apps/desktop/scripts/README.md](apps/desktop/scripts/README.md)

## Repository status

The product is under active construction. Public releases ship only after
security, accessibility, Windows qualification, packaging, and recovery
matrices pass. Implemented surfaces and blockers:
[docs/quality/implementation-status.md](docs/quality/implementation-status.md).
Release gates:
[docs/quality/release-qualification.md](docs/quality/release-qualification.md).

## Contributing

Outside code contributions are not accepted; see [CONTRIBUTING.md](CONTRIBUTING.md)
for issue policy and engineering standards. Security reports go to
[SECURITY.md](SECURITY.md), not public issues.

## Licensing

Copyright (c) 2026 Grok Insider. Grok Desktop is available under the GNU Affero
General Public License v3.0 or later, or under a separate commercial license.
See [LICENSE](LICENSE) and [COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md).
