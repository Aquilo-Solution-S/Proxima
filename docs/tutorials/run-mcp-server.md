# Run the MCP Server

## Prerequisites

Follow [Local Development Quickstart](../getting-started/local-dev.md) through
`Start Postgres` and `Configure Auth`.

## Start Loopback Server

```sh
export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima
cargo run -p proxima-mcp
```

Expected: server listens on `http://127.0.0.1:31415/mcp`.
MCP clients authenticate with an OIDC bearer and select the session owner
during `initialize` with `X-Proxima-Owner: personal:$USER_ID`.

## Tool Surface Profiles

The default substrate exposes memory, goals, citations, citation-only Fact
actions, membership/profile-scoped administration, and introspection tools.
`PROXIMA_TOOL_PROFILE=memory` shrinks the advertised surface for agent memory use.

```sh
PROXIMA_TOOL_PROFILE=memory cargo run -p proxima-mcp
```

## Network Exposure

Loopback is the default. Non-loopback binds require `PROXIMA_EXPOSE_NETWORK=true`
and the auth/origin/host gates described in [../reference/env-vars.md](../reference/env-vars.md)
and [../15-deployment.md](../15-deployment.md).
