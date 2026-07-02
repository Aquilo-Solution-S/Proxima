# proxima-mcp

Headless MCP host binary for Proxima.

## Run Locally

```sh
export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima
cargo run -p proxima-mcp -- --master-token "$MASTER_TOKEN" --master-token-subject "$USER_ID"
```

## Auth

- Loopback development: master token + subject UUID.
- MCP initialize: send `X-Proxima-Owner: personal:<USER_ID>` or another authorized owner key.
- Production: OIDC/host authenticator per [`../../docs/10-configuration.md`](../../docs/10-configuration.md) and [`../../docs/15-deployment.md`](../../docs/15-deployment.md).

## Tool Profiles

`PROXIMA_TOOL_PROFILE=memory` narrows the advertised MCP surface for memory-focused agents.

## Discovery

Clients must read `initialize.instructions`, `tools/list`, resources, `proxima://how-to`, and `proxima://tools`.
