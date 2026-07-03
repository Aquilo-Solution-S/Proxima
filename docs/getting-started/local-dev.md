# Local Development Quickstart

## Prerequisites

- Rust toolchain from `rust-toolchain.toml`.
- Docker or compatible container runtime.
- `python3` for UUID generation snippets.
- An OIDC issuer/client that can mint a bearer for your local MCP client.
- `psql` optional for manual DB inspection.

## Start Postgres

```sh
docker compose -f docker-compose.dev.yml up -d --wait postgres
```

The dev compose file exposes pgvector Postgres at `localhost:5434`. If your
Compose implementation lacks `--wait`, run `docker compose -f docker-compose.dev.yml ps`
until the Postgres service is healthy before starting the server.

## Configure Auth

```sh
USER_ID=$(python3 - <<'PY'
import uuid
print(uuid.uuid4())
PY
)
export PROXIMA_OIDC_ISSUER=https://idp.example.test
export PROXIMA_OIDC_AUDIENCE=proxima-mcp
export PROXIMA_PUBLIC_URL=http://127.0.0.1:31415
export PROXIMA_OIDC_SUBJECT_MAP=sub-from-idp:$USER_ID
printf 'USER_ID=%s\n' "$USER_ID"
```

## Run MCP Server

```sh
export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima
cargo run -p proxima-mcp
```

Expected: server listens on `http://127.0.0.1:31415/mcp`.
MCP clients must send a valid OIDC bearer plus
`X-Proxima-Owner: personal:$USER_ID` on `initialize`.

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

## Next

- Configure a coding agent: [connect-agent.md](connect-agent.md)
- Troubleshoot failures: [../how-to/troubleshoot.md](../how-to/troubleshoot.md)
