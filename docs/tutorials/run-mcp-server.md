# Run the MCP Server

## Prerequisites

Follow [Local Development Quickstart](../getting-started/local-dev.md) through
Postgres and the local OIDC issuer.

## Start Loopback Server

```sh
export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima
export PROXIMA_TOOL_PROFILE=full
cargo run -p proxima-mcp
```

Expected: server listens on `http://127.0.0.1:31415/mcp` with the code
flavor linked. MCP clients authenticate with an OIDC bearer and select the
session owner during `initialize` with `X-Proxima-Owner: personal:$USER_ID`.

Substrate-only (no code flavor): `cargo run -p proxima-mcp --no-default-features`.

## Tool Surface Profiles

The binary default is fail-closed `memory` (hides `core_membership` /
`core_publish`). Local full capability uses `PROXIMA_TOOL_PROFILE=full`.

```sh
PROXIMA_TOOL_PROFILE=memory cargo run -p proxima-mcp
```

## Network Exposure

Loopback is the default. Non-loopback binds require `PROXIMA_EXPOSE_NETWORK=true`
and the auth/origin/host gates described in [../reference/env-vars.md](../reference/env-vars.md)
and [../15-deployment.md](../15-deployment.md).
