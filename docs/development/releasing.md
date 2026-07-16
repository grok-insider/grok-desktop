# Release operations

`master` is the released line and `dev` is the integration line. Human changes
land in `dev` first. Only `dev`, `release-please--*`, and
`release-please-manual-*` pull requests may merge into `master`.

## Automated patch releases

After a pull request merges into `master`, Release Please opens or updates one
standing release pull request. The first proposal is `v0.0.1`; later automatic
proposals increment only the patch component. Every `0.0.z` release is an
explicit GitHub prerelease on the beta channel even though its SemVer has no
suffix. The workflow synchronizes the
Node, Electron, Cargo workspace, internal Rust dependency, Cargo lock, and Nix
versions before adding bounded user-facing highlights to `CHANGELOG.md`.

The release pull request uses `RELEASE_PLZ_TOKEN`, a fine-grained token with
repository Contents and Pull requests read/write permission. This token is
required so commits created by the release workflow run the normal protected
CI checks. It is not a Release Please vendor token: it is an owner-managed
GitHub token stored under that repository secret name. `OPENROUTER_API_KEY` is
optional: without it, the deterministic
Release Please notes remain unchanged.

Do not merge a release pull request until the protected release prerequisites
pass. Release Please intentionally skips GitHub release creation. After the
approved release PR merges, **Finalize approved release** revalidates the
manifest, synchronized versions, changelog, branch, and title before pushing
the immutable tag with the owner token. That authenticated tag event starts the
artifact workflow; the tag workflow creates the GitHub Release only after every
artifact and manifest is signed and verified. Exact artifact qualification then
occurs at the protected `beta-release` promotion hold before publication.

If the finalizer fails after the release PR has merged and before it creates a
tag, fix and promote the finalizer first. An owner may then dispatch **Finalize
approved release** with the exact current `master` SHA and synchronized version.
The recovery path refuses a stale or non-`master` commit and re-runs the same
version, manifest, changelog, and tag-absence checks. Never use it to replace a
failed artifact build or to move an existing tag.

## Manual milestones

Repository administrators use **Manual Version Bump** for a deliberate minor
or major release. The workflow fails before the initial release, rejects
non-administrators, opens an approved `release-please-manual-*` pull request,
and creates the tag only after that protected pull request merges.
Release Please never changes the major or minor component automatically.

## Protected environments

Stable and beta use separate environment approvals and channel-scoped signing
material:

- `stable-windows-signing` and `stable-release`
- `beta-windows-signing` and `beta-release`
- `beta-build` for unprivileged Linux preview assembly before promotion

The beta Windows package uses the separate identity
`GrokInsider.GrokDesktop.Preview`, publisher `CN=Grok Desktop Preview`, and
display name `Grok Desktop Preview`. Its protected environment additionally
owns `GROK_WINDOWS_PREVIEW_CERT_PFX_BASE64` and
`GROK_WINDOWS_PREVIEW_CERT_PASSWORD`. Those secrets exist only during the
certificate-import step on an ephemeral worker; packaging receives only the
certificate-store thumbprint. The public `.cer` and its SHA-256 digest are
release assets. Preview users explicitly install it into the current-user
Trusted People store. A future publicly trusted stable identity is separate
and may require uninstall/reinstall.

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

The `beta-release` environment is the promotion hold. Linux and Windows build
artifacts remain workflow artifacts for seven days while the exact bytes are
qualified. Approving publication signs update metadata, creates SPDX SBOM,
checksums, GitHub artifact attestations, release evidence, and the prerelease.
Do not approve `v0.0.1` until Wisp/CDP Linux QA and clean-VM Windows QA refer to
the exact workflow run and artifact hashes.

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
