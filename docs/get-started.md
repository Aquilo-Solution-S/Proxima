# Get started
> Status: current.

This is the shortest complete local path: pgvector Postgres, the loopback
issuer, the MCP host, and one lexical memory round trip. It needs no embedding
service. For the complete configuration inventory see
[configuration](configuration.md); for a client-specific connection see
[host integration](host-integration.md).

## Prerequisites

Install the Rust toolchain named by `rust-toolchain.toml`, Docker Compose, and
an MCP-capable client. `curl` and `jq` are useful for HTTP probes. Work from
the repository root.

## 1. Boot Postgres

The development image supplies `vector`, `pg_trgm`, and `btree_gin`. The
bootstrap script creates separate, non-superuser serving and platform roles.
Use the same port in every shell. The default is `5434`.

```sh
export PROXIMA_DEV_POSTGRES_PORT=5434
docker compose -f docker-compose.dev.yml up -d --wait postgres
docker compose -f docker-compose.dev.yml exec -T postgres \
  psql -U proxima -d proxima -v ON_ERROR_STOP=1 < scripts/pg-init.sql
export DATABASE_URL="postgres://proxima_runtime:proxima-runtime-dev@localhost:${PROXIMA_DEV_POSTGRES_PORT}/proxima"
export PROXIMA_PLATFORM_DATABASE_URL="postgres://proxima_platform:proxima-platform-dev@localhost:${PROXIMA_DEV_POSTGRES_PORT}/proxima"
export PROXIMA_STORE_NUMBER=0
```

`DATABASE_URL` is the runtime connection. `PROXIMA_PLATFORM_DATABASE_URL` is
the migration and maintenance connection. Keep both set: owner-RLS boot and
the migration runner require the split. The sample credentials are loopback
development credentials from [`scripts/pg-init.sql`](https://github.com/Aquilo-Solution-S/Proxima/blob/main/scripts/pg-init.sql).

## 2. Start the local issuer

In a second shell, run:

```sh
cargo run -p proxima-dev-idp
```

The command prints an issuer, audience, subject map, bearer, and a user UUID.
Copy the printed `PROXIMA_OIDC_*` values into the server shell. Set the owner
to the raw UUID printed after `-> user`; the owner header is not a
`personal:<uuid>` string.

```sh
export PROXIMA_OIDC_ISSUER="<issuer printed by dev-idp>"
export PROXIMA_OIDC_AUDIENCE="<audience printed by dev-idp>"
export PROXIMA_OIDC_SUBJECT_MAP_JSON='<issuer-aware JSON map printed by dev-idp>'
export PROXIMA_PUBLIC_URL="http://127.0.0.1:31415"
export PROXIMA_TOOL_PROFILE=full
export OWNER_ID="<raw-user-uuid>"
export ACCESS_TOKEN="<token printed by dev-idp>"
```

`dev-idp` binds to loopback and is for development only. It stores its signing
key under `~/.proxima/dev-idp.pkcs8` with mode `0600`; never expose or deploy
it. Use `--ephemeral` for a disposable issuer with an in-memory signing key.

## 3. Start Proxima

In the shell containing the database and issuer exports:

```sh
cargo run -p proxima-mcp
```

The host serves `http://127.0.0.1:31415/mcp`. A successful MCP
`initialize` response carries `Mcp-Session-Id`. Both authentication headers
are required on client requests:

```sh
curl -sS -D- -o/dev/null -X POST "$PROXIMA_PUBLIC_URL/mcp" \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H "Authorization: Bearer $ACCESS_TOKEN" \
  -H "X-Proxima-Owner: $OWNER_ID" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"1"}}}'
```

## 4. Connect an agent

For Claude Code, use the values printed by `dev-idp`:

```sh
claude mcp add --transport http proxima http://127.0.0.1:31415/mcp \
  --header "Authorization: Bearer $ACCESS_TOKEN" \
  --header "X-Proxima-Owner: $OWNER_ID"
```

Other clients use the same URL and headers. Read `tools/list` after
`initialize`; the running catalog is the authority for available tools.

## 5. Remember and search without embeddings

Call `core_remember` with a title, body, and idempotency key. Then call
`core_search_memories` with the same distinctive words and `mode: "lexical"`.
The first call returns a Fact handle; the second returns the matching memory.
These JSON-RPC bodies can be pasted into an MCP client:

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"core_remember","arguments":{"title":"Local quickstart check","body":"Proxima lexical memory works without an embedding service.","idempotency_key":"local-quickstart-lexical-1"}}}
```

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"core_search_memories","arguments":{"query":"lexical memory embedding service","mode":"lexical","limit":5}}}
```

No `PROXIMA_EMBED_*` variables are needed for this path. Semantic search
requires an embedding client; lexical search remains available without one.

## Troubleshooting

- `DATABASE_URL` or `PROXIMA_PLATFORM_DATABASE_URL` is missing: export both
  values from step 1 and restart the host.
- `401 Unauthorized`: verify the bearer, issuer, audience, subject map, and
  raw UUID owner header came from the same `dev-idp` process.
- Postgres is unhealthy: run `docker compose -f docker-compose.dev.yml ps`,
  then replay `scripts/pg-init.sql` after the service is healthy.
- Port `5434` is occupied: choose another `PROXIMA_DEV_POSTGRES_PORT` before
  Compose starts and rebuild both URLs with that value.

Continue with [host integration](host-integration.md), or read the full
[configuration reference](configuration.md).
