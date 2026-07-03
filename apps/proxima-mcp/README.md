# proxima-mcp

Headless MCP host binary for Proxima.

## Run Locally

```sh
export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima
export PROXIMA_PUBLIC_URL=http://127.0.0.1:31415
export PROXIMA_OIDC_ISSUER=https://idp.example.test
export PROXIMA_OIDC_AUDIENCE=proxima-mcp
export PROXIMA_OIDC_SUBJECT_MAP=sub-from-idp:<user-uuid>
cargo run -p proxima-mcp
```

## Auth

- MCP bearer auth is host/OIDC only.
- MCP initialize: send `X-Proxima-Owner: personal:<USER_ID>` or another authorized owner key.
- Configure OIDC/host auth per [`../../docs/10-configuration.md`](../../docs/10-configuration.md) and [`../../docs/15-deployment.md`](../../docs/15-deployment.md).

## Tool Profiles

`PROXIMA_TOOL_PROFILE=memory` narrows the advertised MCP surface for memory-focused agents.

## Discovery

Clients must read `initialize.instructions`, `tools/list`, resources, `proxima://how-to`, and `proxima://tools`.
