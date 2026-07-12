# Contributing

Grok Desktop currently does **not** accept outside code contributions. This
keeps copyright ownership unambiguous for the project's AGPL/commercial
dual-license model.

You may open issues for:

- Reproducible defects
- Accessibility problems
- Security hardening ideas (non-exploit writeups; see [SECURITY.md](SECURITY.md)
  for vulnerabilities)
- Supported-contract questions and product design feedback

Do **not** submit secrets, provider credentials, private account exports,
copyrighted datasets, or reverse-engineered Grok endpoints.

Security vulnerabilities must follow [SECURITY.md](SECURITY.md) instead of a
public issue.

## Engineering standards

Even when code is not accepted from outside, work in this repository—by
maintainers or AI agents—follows the same standards:

| Topic | Doc |
|-------|-----|
| Documentation map | [docs/README.md](docs/README.md) |
| Product invariants | [AGENTS.md](AGENTS.md) |
| Clean Architecture, SOLID, clean code | [docs/architecture/principles.md](docs/architecture/principles.md) |
| Module ownership | [docs/architecture/modules.md](docs/architecture/modules.md) |
| Coding checklists | [docs/development/coding-guidelines.md](docs/development/coding-guidelines.md) |
| Local run and gates | [docs/development/local-development.md](docs/development/local-development.md) |
| Debug and QA | [docs/development/debugging-and-qa.md](docs/development/debugging-and-qa.md) |
| UI design system | [apps/desktop/DESIGN.md](apps/desktop/DESIGN.md) |
| ADRs | [docs/decisions/README.md](docs/decisions/README.md) |

Useful issue reports include: OS and arch, how you built or ran from source,
exact steps, expected vs actual behavior, and logs with secrets redacted.

## Licensing

Contributions, if the policy ever opens, would need to be compatible with the
AGPL-3.0-or-later / commercial dual-license. See [LICENSE](LICENSE) and
[COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md).
