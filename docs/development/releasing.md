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
and `release-candidate/v1` check pass. Release Please intentionally skips GitHub
release creation. **Release candidate** validates the exact same-repository,
up-to-date Release Please head and permits only generated changelog/version
changes before invoking the production Linux and Windows build workflow.
**Qualify release candidate** binds clean-machine QA to the exact source tree,
workflow attempt, Actions artifact IDs, archive digests, and payload hashes.

After the qualified release PR merges, **Finalize approved release** downloads
and independently validates that protected qualification record and proves the
merged tree equals the tested tree before pushing the immutable tag. The
finalizer then replaces Release Please's `autorelease: pending` label with
`autorelease: tagged`. The tag workflow downloads and promotes those exact
qualified artifacts without rebuilding them; `beta-release` remains the final
metadata-signing and publication hold.

If finalization fails, an owner may dispatch **Finalize approved release** with
the exact merged release PR, qualification run, current `master` SHA, and
synchronized version. Recovery accepts an existing tag only when it already
points at that exact qualified commit, allowing a failed lifecycle-label step
to finish without moving or replacing the tag. A transient publication failure
may similarly redispatch **Public release** with the existing tag and its bound
qualification run; it promotes the same artifact IDs and cannot rebuild.

## Manual milestones

Repository administrators use **Manual Version Bump** for a deliberate minor
or major release. The workflow fails before the initial release, rejects
non-administrators, opens an approved `release-please-manual-*` pull request,
and creates the tag only after that protected pull request merges.
Release Please never changes the major or minor component automatically.

## Protected environments

Stable and beta use separate environment approvals and channel-scoped update
signing material:

- `stable-windows-build` and `stable-release`
- `beta-windows-build` and `beta-release`
- `beta-build` for unprivileged Linux preview assembly before promotion
- `beta-candidate` for secret-free owner approval of exact QA evidence

The public Windows core package is an intentionally unsigned, per-user NSIS
installer. It does not require a PFX, certificate-store identity, SignTool, or
timestamp service. Windows can display Unknown Publisher or Microsoft Defender
SmartScreen warnings; release notes direct users to the immutable source tag,
`SHA256SUMS`, and GitHub artifact attestation before they bypass the warning.
The signed update manifest remains a separate mandatory trust boundary.

The Windows environment owns the documented Windows Cargo/Rust/MSVC build-tool
paths and bounded toolchain environment, `GROK_UPDATE_TRUSTED_KEYS_JSON`, and
xAI component evidence variables. The
publish environment owns
`GROK_UPDATE_SIGNING_KEY_ID`, `GROK_UPDATE_TRUSTED_KEYS_JSON`, and the
`GROK_UPDATE_SIGNING_PRIVATE_KEY_PEM` secret. Values never belong in source,
logs, artifacts, or broad runner environments.

The Windows job also requires a qualified, ephemeral runner with labels
`self-hosted`, `windows`, and `x64`. The absence of any runner, environment
approval, build input, update trust, or redistribution evidence must stop the
release before publication.

Before registering that runner, remove the previous candidate's Actions
workspace with an administrator identity and recreate the runner work root
with ACL inheritance for the account that will run `Runner.Worker`. Reusing a
workspace across runner identities is forbidden: packaged Electron files can
retain a narrower ACL and make `actions/checkout` loop while trying to remove
an inaccessible stale tree. Toolchain and explicitly documented dependency
caches may persist outside the workspace; source trees, build outputs, runner
diagnostics, and registration state may not.

Linux and Windows candidate artifacts remain workflow artifacts for 30 days;
the qualification record remains for 90 days and is copied into the public
release. The `beta-release` environment is the final promotion hold. Approving
publication signs update metadata, creates an SPDX SBOM, checksums, GitHub
artifact attestations, release evidence, and the prerelease. Do not qualify a
candidate until Wisp/CDP Linux QA and clean-VM Windows QA refer to the exact
workflow run, attempt, artifact IDs, and payload hashes.

Windows Wisp/VM integration is deferred for the initial preview. Windows
release QA uses a fresh libvirt overlay, QGA guest execution, and an in-guest
loopback CDP probe. This exception does not permit skipping artifact identity,
runtime portability, install/start/IPC, deep-link, repair, or uninstall checks.
Linux native QA continues in Wisp's hidden compositor.

## Operator checks

Before dispatching or merging a release pull request:

```sh
pnpm release:check-version
pnpm check
```

Then confirm that the release PR is current with `master`, every required check
is green, the protected environments contain the correct channel inputs, and
the qualified Windows worker is online. Confirm the candidate workspace was
recreated for the current runner service identity before registration. Never
create or repair a release tag manually outside the documented workflows.

Release tool downloads must use immutable versioned release assets with a
tracked SHA-256. Do not use rolling or `continuous` asset URLs: a byte change
after tagging makes the release irreproducible and must fail closed.

Arguments forwarded through a filtered pnpm package command are resolved from
that package's working directory. Release workflows must pass
`release/components/...` to desktop package scripts while retaining
`apps/desktop/release/components/...` for root-level workflow steps; contract
tests pin both forms to prevent duplicated `apps/desktop/apps/desktop` paths.
Do not add pnpm's optional `--` separator to `package:windows-core`: pnpm forwards
it to the strict release parser as an argument. Options following the script
name are already forwarded.

The dormant isolated/enterprise Windows train retains a separately qualified
signed-MSIX design. Its certificate, identity, privileged service, and guest
requirements are deferred and are not prerequisites for the public core NSIS
release.
