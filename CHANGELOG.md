# Changelog

## v0.0.2

- **Breaking (MCP wire):** action-dispatch tools (`core_goal`, `core_wake`,
  `core_personality`, `core_fact`) now reject unknown argument fields before
  deserialization with JSON-RPC `-32602`, instead of silently ignoring them.
  Clients must send only the fields an action declares.
- MCP dispatcher argument schemas are now MCP/OpenAI-compliant: object root
  with a top-level `properties` object and no root `oneOf`/`anyOf`/`allOf`.
  Internally-tagged variants are flattened into one object keyed by an
  `action` enum discriminator, with per-action field metadata exposed under
  the `x-proxima-actions` schema extension (`allowed_fields`, `required_fields`,
  `field_descriptions`) and mirrored in the `proxima://tools` catalog.
- `core_goal` payload `body` now defaults to `{}` (was `null`) when omitted.
- Dependency bumps: `jsonwebtoken` 9.3.1 → 10.4.0, `sha2` 0.10.9 → 0.11.0,
  `tree-sitter` 0.25.10 → 0.26.9, plus a cargo minor/patch group (6 updates).
- CI/infra: `actions/checkout` 4 → 7, `contributor-assistant/github-action`
  bump, and a fix to the CLA Assistant configuration.

## v0.0.1

- Substrate MCP surface: 9 core tools and 7 resources.
- Code flavor MCP surface: 10 additional tools when built into the host.
- Streamable HTTP MCP server with OIDC bearer-token authentication.
- S3 cited-blob support for large cited artefacts.
- Postgres storage with pgvector/HNSW semantic search.
- Rust framework and flavor composition consumed from the git workspace.
- Git tag is `v0.0.1`; workspace crate versions are `0.1.0` with `publish = false`, not a published crates.io contract.
