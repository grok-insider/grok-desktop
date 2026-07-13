# Release operations

`master` is the released line and `dev` is the integration line. Human changes
land in `dev` first. Only `dev`, `release-please--*`, and
`release-please-manual-*` pull requests may merge into `master`.

## Automated patch releases

After a pull request merges into `master`, Release Please opens or updates one
standing release pull request. The first proposal is `v0.1.0`; later automatic
proposals increment only the patch component. The workflow synchronizes the
Node, Electron, Cargo workspace, internal Rust dependency, Cargo lock, and Nix
versions before adding bounded user-facing highlights to `CHANGELOG.md`.

The release pull request uses `RELEASE_PLZ_TOKEN`, a fine-grained token with
repository Contents and Pull requests read/write permission. This token is
required so commits created by the release workflow run the normal protected
CI checks. `OPENROUTER_API_KEY` is optional: without it, the deterministic
Release Please notes remain unchanged.

Do not merge a release pull request until the exact artifact has passed
[release qualification](../quality/release-qualification.md). Merging creates
the immutable tag; the tag workflow, not Release Please, creates the GitHub
Release after every artifact and manifest is signed and verified.

## Manual milestones

Repository administrators use **Manual Version Bump** for a deliberate minor
or major release. The workflow fails before the initial release, rejects
non-administrators, opens an approved `release-please-manual-*` pull request,
and creates the tag only after that protected pull request merges.

## Protected environments

Stable and beta use separate environment approvals and channel-scoped signing
material:

- `stable-windows-signing` and `stable-release`
- `beta-windows-signing` and `beta-release`

The Windows environment owns the documented `GROK_MSIX_*`,
`GROK_WINDOWS_*`, `GROK_RELEASE_METADATA_PUBLIC_KEYS_JSON`,
`GROK_ACP_CATALOG_TRUSTED_KEYS`, `GROK_UPDATE_TRUSTED_KEYS_JSON`, and xAI
component evidence variables. The publish environment owns
`GROK_UPDATE_SIGNING_KEY_ID`, `GROK_UPDATE_TRUSTED_KEYS_JSON`, and the
`GROK_UPDATE_SIGNING_PRIVATE_KEY_PEM` secret. Values never belong in source,
logs, artifacts, or broad runner environments.

The Windows job also requires a qualified, ephemeral runner with labels
`self-hosted`, `windows`, and `x64`. The absence of any runner, environment
approval, signing input, update trust, or redistribution evidence must stop the
release before publication.

## Operator checks

Before dispatching or merging a release pull request:

```sh
pnpm release:check-version
pnpm check
```

Then confirm that the release PR is current with `master`, every required check
is green, the protected environments contain the correct channel inputs, and
the qualified Windows worker is online. Never create or repair a release tag
manually outside the documented workflows.
