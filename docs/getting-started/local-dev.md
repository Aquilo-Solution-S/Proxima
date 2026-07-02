# Local Development Quickstart

## Prerequisites

- Rust toolchain from `rust-toolchain.toml`.
- Docker or compatible container runtime.
- `python3` for UUID generation snippets.
- `psql` optional for manual DB inspection.

## Start Postgres

```sh
docker compose -f docker-compose.dev.yml up -d --wait postgres
```

The dev compose file exposes pgvector Postgres at `localhost:5434`. If your
Compose implementation lacks `--wait`, run `docker compose -f docker-compose.dev.yml ps`
until the Postgres service is healthy before starting the server.

## Generate Dev IDs

```sh
USER_ID=$(python3 - <<'PY'
import uuid
print(uuid.uuid4())
PY
)
MASTER_TOKEN=$(python3 - <<'PY'
import uuid
print(uuid.uuid4())
PY
)
printf 'USER_ID=%s\nMASTER_TOKEN=%s\n' "$USER_ID" "$MASTER_TOKEN"
```

## Run MCP Server

```sh
export DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima
cargo run -p proxima-mcp -- --master-token "$MASTER_TOKEN" --master-token-subject "$USER_ID"
```

Expected: server listens on `http://127.0.0.1:31415/mcp`.
MCP clients must send `X-Proxima-Owner: personal:$USER_ID` on `initialize`.

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
