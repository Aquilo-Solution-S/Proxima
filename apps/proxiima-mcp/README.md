# proxiima-mcp

Canonical Proxiima host: Streamable HTTP MCP server. This is the binary to run locally with a
coding agent and the wiring template for other hosts.

## Run Locally

Follow [`docs/get-started.md`](../../docs/get-started.md):
Postgres, `proxiima-dev-idp`, then:

```sh
export DATABASE_URL="postgres://proxiima:proxiima@localhost:${PROXIIMA_DEV_POSTGRES_PORT:-5434}/proxiima"
export PROXIIMA_STORE_NUMBER=0
export PROXIIMA_TOOL_PROFILE=full
cargo run -p proxiima-mcp
```

`dev-idp` prints the OIDC exports and a ready-to-paste agent command.
Listens at `http://127.0.0.1:31415/mcp`.

## Auth

- MCP bearer auth is host/OIDC only. Local means a local issuer, not a bypass.
- MCP initialize: send `X-Proxiima-Owner: personal:<USER_ID>` or another authorized owner key.
- Configure OIDC/host auth per [`docs/configuration.md`](../../docs/configuration.md) and [`docs/operate.md`](../../docs/operate.md).

## Tool Profiles

Default is fail-closed `memory`. Local full surface (including
`core_membership` / `core_transfer`): `PROXIIMA_TOOL_PROFILE=full`. Any other
value, `code` included, is refused at startup.

## Discovery

Clients must read `initialize.instructions`, `tools/list`, resources, `proxiima://how-to`, and `proxiima://tools`.
