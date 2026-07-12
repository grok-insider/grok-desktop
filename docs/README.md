# Documentation map

Start here if you are new to the repository. Canonical agent checklist:
[AGENTS.md](../AGENTS.md).

## Reading paths

### New agent or maintainer

1. [AGENTS.md](../AGENTS.md) — invariants, commands, UI/edit rules  
2. [architecture/principles.md](architecture/principles.md) — Clean Architecture, SOLID, clean code  
3. [architecture/modules.md](architecture/modules.md) — where code lives  
4. [development/local-development.md](development/local-development.md) — run and test  
5. [development/debugging-and-qa.md](development/debugging-and-qa.md) — CDP, e2e, Wisp  

### UI change

1. [apps/desktop/DESIGN.md](../apps/desktop/DESIGN.md)  
2. [development/coding-guidelines.md](development/coding-guidelines.md) (UI checklist)  
3. [development/debugging-and-qa.md](development/debugging-and-qa.md)  

### Daemon / domain / protocol change

1. [architecture/principles.md](architecture/principles.md)  
2. [architecture/overview.md](architecture/overview.md)  
3. [architecture/protocol-and-persistence.md](architecture/protocol-and-persistence.md)  
4. Relevant [ADR](decisions/README.md)  
5. [development/coding-guidelines.md](development/coding-guidelines.md)  

### Security / platform

1. [platform/threat-model.md](platform/threat-model.md)  
2. [SECURITY.md](../SECURITY.md)  
3. [platform/windows-release.md](platform/windows-release.md)  
4. [platform/adr/](platform/adr/) (guest, channel, privileged journal)  

### Release readiness

1. [quality/implementation-status.md](quality/implementation-status.md)  
2. [quality/release-qualification.md](quality/release-qualification.md)  

## Catalog

### Architecture

| Doc | Description |
|-----|-------------|
| [architecture/overview.md](architecture/overview.md) | Process model, authority, platform strategy |
| [architecture/principles.md](architecture/principles.md) | Engineering principles (CA, SOLID, clean code) |
| [architecture/modules.md](architecture/modules.md) | Crate and app ownership map |
| [architecture/protocol-and-persistence.md](architecture/protocol-and-persistence.md) | IPC epochs and SQLCipher chronicle |

### Development

| Doc | Description |
|-----|-------------|
| [development/local-development.md](development/local-development.md) | Bootstrap, dev loops, gates |
| [development/debugging-and-qa.md](development/debugging-and-qa.md) | CDP, e2e, Hyprland, Wisp |
| [development/coding-guidelines.md](development/coding-guidelines.md) | Checklists and definition of done |
| [apps/desktop/scripts/README.md](../apps/desktop/scripts/README.md) | CDP harness implementation detail |

### Decisions and quality

| Doc | Description |
|-----|-------------|
| [decisions/README.md](decisions/README.md) | Product ADR index |
| [quality/implementation-status.md](quality/implementation-status.md) | What is implemented vs blocked |
| [quality/release-qualification.md](quality/release-qualification.md) | Release gates |

### Platform and research

| Doc | Description |
|-----|-------------|
| [platform/threat-model.md](platform/threat-model.md) | Execution threat model |
| [platform/windows-release.md](platform/windows-release.md) | MSIX, signing, fuses |
| [platform/adr/](platform/adr/) | Privileged guest ADRs |
| [research/official-grok-surfaces.md](research/official-grok-surfaces.md) | Official Grok contracts |
| [research/claude-cowork-windows.md](research/claude-cowork-windows.md) | Cowork reference deltas |

### Product integrations

| Doc | Description |
|-----|-------------|
| [integrations/README.md](../integrations/README.md) | Managed integration layout |
| [integrations/first-party/wisp/ADAPTER.md](../integrations/first-party/wisp/ADAPTER.md) | Product Wisp adapter |

### Working plans (non-canonical)

| Doc | Description |
|-----|-------------|
| [ai-continue-plan.md](../ai-continue-plan.md) | Historical agent handoff log |
| [ui-plan.md](../ui-plan.md) | Historical UI migration plan |

Prefer `docs/` over working plans when instructions disagree.
