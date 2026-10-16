# proxima-mcp

Canonical Proxima host: Streamable HTTP MCP server. This is the binary to run locally with a
coding agent and the wiring template for other hosts.

## Run Locally

Follow [`docs/get-started.md`](../../docs/get-started.md):
Postgres, `proxima-dev-idp`, then:

```sh
export DATABASE_URL="postgres://proxima:proxima@localhost:${PROXIMA_DEV_POSTGRES_PORT:-5434}/proxima"
export PROXIMA_TOOL_PROFILE=full
cargo run -p proxima-mcp
```

`dev-idp` prints the OIDC exports and a ready-to-paste agent command.
Listens at `http://127.0.0.1:31415/mcp`.

For Code builds and repository administration, see
[`operate`](../../docs/operate.md#container-and-network-boundary).

## Auth

- MCP bearer auth is host/OIDC only. Local means a local issuer, not a bypass.
- MCP initialize: send `X-Proxima-Owner: personal:<USER_ID>` or another authorized owner key.
- Configure OIDC/host auth per [`docs/configuration.md`](../../docs/configuration.md) and [`docs/operate.md`](../../docs/operate.md).

## Tool Profiles

Default is fail-closed `memory`. Local full surface (including
`core_membership` / `core_transfer`): `PROXIMA_TOOL_PROFILE=full`. Code search
without memory tools: `PROXIMA_TOOL_PROFILE=code` (see
[`docs/get-started.md`](../../docs/get-started.md)).

## Discovery

Clients must read `initialize.instructions`, `tools/list`, resources, `proxima://how-to`, and `proxima://tools`.
