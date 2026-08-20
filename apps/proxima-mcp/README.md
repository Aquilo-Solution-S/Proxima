# proxima-mcp

Canonical Proxima host: Streamable HTTP MCP server, code flavor on by
default. This is the binary to run locally with a coding agent and the
wiring template for other hosts.

## Run Locally

Follow [`docs/getting-started/local-dev.md`](../../docs/getting-started/local-dev.md):
Postgres, `proxima-dev-idp`, then:

```sh
export DATABASE_URL="postgres://proxima:proxima@localhost:${PROXIMA_DEV_POSTGRES_PORT:-5434}/proxima"
export PROXIMA_TOOL_PROFILE=full
cargo run -p proxima-mcp
```

`dev-idp` prints the OIDC exports and a ready-to-paste agent command.
Listens at `http://127.0.0.1:31415/mcp`.

Substrate-only (no code flavor): `cargo run -p proxima-mcp --no-default-features`.

## Auth

- MCP bearer auth is host/OIDC only. Local means a local issuer, not a bypass.
- MCP initialize: send `X-Proxima-Owner: personal:<USER_ID>` or another authorized owner key.
- Configure OIDC/host auth per [`docs/10-configuration.md`](../../docs/10-configuration.md) and [`docs/15-deployment.md`](../../docs/15-deployment.md).

## Tool Profiles

Default is fail-closed `memory`. Local full surface (including
`core_membership` / `core_transfer`): `PROXIMA_TOOL_PROFILE=full`.

## Discovery

Clients must read `initialize.instructions`, `tools/list`, resources, `proxima://how-to`, and `proxima://tools`.
