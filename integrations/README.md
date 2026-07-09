# Managed integrations

Managed integrations are signed bundles executed by the NixOS guest integration
runner. `schema/integration-manifest.schema.json` is the install-time contract;
unknown fields are rejected so a misspelled permission cannot broaden access.
`schema/integration-catalog-v1.schema.json` defines the release inventory that
binds those manifests and every bundle file to its digest, size, and executable
bit. Release tooling additionally enforces canonical ordering, cross-field
manifest binding, portable paths, and aggregate limits that JSON Schema cannot
express.
The runtime loader mirrors the schema in typed validation, including required
empty arrays, signature metadata, permission bounds, canonical guest paths,
read-only/read-write overlap, network endpoints, process allowlists, and
lifecycle ranges. JSON Schema validation alone is not treated as enforcement.

Adapters use newline-delimited JSON over dedicated stdin/stdout. Their lifecycle
is independent from the desktop process, and every granted permission is the
intersection of the signed manifest, user approval, and guest policy. Standard
error is diagnostic-only. The integration manager must redact secrets and user
content before persisting logs. Lifecycle envelopes validate against
`schema/managed-adapter-protocol-v1.schema.json`; computer-use messages on the
same stream validate independently against the computer-use schema.

Computer use is defined in `schema/computer-use-v1.schema.json`. Actions are a
closed union and carry both the observation revision and application identity.
There is no arbitrary process or shell execution action.

The Wisp bundle under `first-party/wisp` is the first-party reference adapter and
the recommended computer-use add-on. Its source manifest remains unsigned on the
development channel; release packaging supplies the signature. Release tooling
uses `native/windows-vm-service/manifestverify.SigningBytes` as the canonical
Ed25519 signing payload, and the loader applies the same publisher key,
capability, path, and protocol checks before installation.
