# Troubleshooting

Day-2 backup/restore, failed-migration behavior, and embedding
signal→action runbook: [operate.md](operate.md).

| Symptom | Check | Fix |
|---|---|---|
| `DATABASE_URL` connects to wrong DB | The URL port differs from `PROXIMA_DEV_POSTGRES_PORT` | export `DATABASE_URL="postgres://proxima:proxima@localhost:${PROXIMA_DEV_POSTGRES_PORT:-5434}/proxima"` |
| Postgres host port already used | `docker compose -f docker-compose.dev.yml config` or `lsof -i :${PROXIMA_DEV_POSTGRES_PORT:-5434}` | set `PROXIMA_DEV_POSTGRES_PORT` before `docker compose up` and use the same value in `DATABASE_URL` |
| RustFS host port already used | `docker compose -f docker-compose.dev.yml config` or `lsof -i :${PROXIMA_DEV_S3_PORT:-9100}` | set `PROXIMA_DEV_S3_PORT` before `docker compose up`; point `PROXIMA_S3_ENDPOINT_URL` at the selected host port |
| Postgres not ready | `docker compose -f docker-compose.dev.yml ps` | wait for the healthy service, then rerun `docker compose -f docker-compose.dev.yml up -d --wait postgres` |
| port `31415` already used | `lsof -i :31415` | stop the other process or set `PROXIMA_MCP_BIND` |
| MCP auth rejected | bearer token, OIDC subject map, or owner selection mismatch | use a valid OIDC bearer and `X-Proxima-Owner: personal:<user-id>` / `group:<uuid>` |
| semantic search looks lexical | no embedding client configured or queue not drained | use lexical/hybrid search or configure embeddings per [docs/10-configuration.md](../10-configuration.md) |
| clippy fails on warnings | workspace denies warnings and pedantic lints | fix warning; do not allow/ignore unless design-approved |
| rustdoc fails | broken intra-doc link or warning | fix link/doc warning; CI uses `RUSTDOCFLAGS=-D warnings` |
