# Local Development Quickstart

Everything on this page runs on one machine with no hosted service and no
account anywhere: your Postgres, your OIDC issuer, your embedding model.

## Prerequisites

- Rust toolchain from `rust-toolchain.toml`.
- Docker or compatible container runtime.
- [Ollama](https://ollama.com) (or any OpenAI-compatible `/embeddings`
  endpoint) if you want semantic search.
- `psql` optional for manual DB inspection.

## Start Postgres

```sh
docker compose -f docker-compose.dev.yml up -d --wait postgres
```

The dev compose file exposes pgvector Postgres at `localhost:5434`. If your
Compose implementation lacks `--wait`, run `docker compose -f docker-compose.dev.yml ps`
until the Postgres service is healthy before starting the server.

## Start A Local OIDC Issuer

Proxima authenticates MCP callers exactly one way: an RS256 bearer, verified
against a JWKS discovered from the configured issuer, with `(iss, sub)`
mapped to a user through an explicit subject map. There is no dev bypass —
adding one would mean the path you test locally is not the path you deploy.

So local development uses a local *issuer*:

```sh
cargo run -p proxima-dev-idp
```

It generates an RSA key, serves `/.well-known/openid-configuration` and a
JWKS on `127.0.0.1:31416`, mints a bearer, and prints the exact environment
and client config to paste. The signing key is stored at
`~/.proxima/dev-idp.pkcs8` (mode 0600) and reused, so the token sitting in
your agent's config survives a restart.

`dev-idp` binds loopback only and refuses anything else. Never deploy it: it
issues a valid identity to anyone who can reach it.

## Run MCP Server

Paste the exports `dev-idp` printed, then:

```sh
export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima
export PROXIMA_TOOL_PROFILE=full
cargo run -p proxima-mcp
```

Expected: server listens on `http://127.0.0.1:31415/mcp` with the code
flavor linked.

`--no-default-features` if you only want the substrate memory tools without
the code-as-memory flavor.

## Local Embeddings

Semantic and hybrid search need an embedding client. Any OpenAI-compatible
`/embeddings` endpoint works, and a loopback one needs no credential:

```sh
ollama pull qwen3-embedding:0.6b
export PROXIMA_EMBED_BASE_URL=http://127.0.0.1:11434/v1
export PROXIMA_EMBED_MODEL=qwen3-embedding:0.6b
```

The model must produce 1024-dimensional vectors — the width of the
`vector(1024)` column that is the substrate's single embedding space.
`qwen3-embedding:0.6b` and `mxbai-embed-large` are 1024 natively. For a
nested-prefix (Matryoshka) model with a wider native width, set
`PROXIMA_EMBED_MATRYOSHKA=true` so the request asks for 1024.

Without an embedding endpoint the server starts in degraded mode:
`proxima://graph.embeddings_client_configured` is `false`, lexical search
works, and semantic/hybrid report the missing capability.

## Verify

```sh
curl -s -D- -o/dev/null -X POST http://127.0.0.1:31415/mcp \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H "Authorization: Bearer $TOKEN" \
  -H "X-Proxima-Owner: personal:$USER_ID" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"probe","version":"1"}}}'
```

Expect `200 OK` with an `Mcp-Session-Id` response header. A `401` with
`WWW-Authenticate: Bearer resource_metadata=...` means the token was absent,
expired, or not verifiable against the issuer's JWKS — check that `dev-idp`
is still running and that `PROXIMA_OIDC_ISSUER` matches the issuer that
minted the token.

## Local Checks

```sh
cargo check --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
```

## Reset Dev Database

```sh
docker compose -f docker-compose.dev.yml down -v
docker compose -f docker-compose.dev.yml up -d --wait postgres
```

## Moving To A Real Issuer

Nothing about the server changes. Point `PROXIMA_OIDC_ISSUER` at Entra,
Zitadel, Auth0, or any provider serving standard JWKS discovery over HTTPS,
set `PROXIMA_OIDC_AUDIENCE` to the resource id it issues tokens for, and map
its subjects with `PROXIMA_OIDC_SUBJECT_MAP_JSON`. Plaintext HTTP is accepted
only for loopback issuers; every other host must be HTTPS.

## Next

- Configure a coding agent: [connect-agent.md](connect-agent.md)
- All configuration knobs: [../10-configuration.md](../10-configuration.md)
- Troubleshoot failures: [../how-to/troubleshoot.md](../how-to/troubleshoot.md)
