#!/usr/bin/env sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repository_root"

check-jsonschema --check-metaschema \
  integrations/schema/*.json \
  integrations/first-party/wisp/config.schema.json

check-jsonschema \
  --schemafile integrations/schema/integration-manifest.schema.json \
  integrations/first-party/wisp/manifest.json

check-jsonschema \
  --schemafile integrations/schema/managed-adapter.schema.json \
  integrations/first-party/wisp/adapter.json

check-jsonschema \
  --schemafile integrations/schema/integration-catalog-v1.schema.json \
  integrations/catalog.example.json
