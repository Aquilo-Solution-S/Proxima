# Troubleshooting

| Symptom | Check | Fix |
|---|---|---|
| `DATABASE_URL` connects to wrong DB | README default differs from compose port | export `DATABASE_URL=postgres://proxima:proxima@localhost:5434/proxima` |
| Postgres not ready | `docker compose -f docker-compose.dev.yml ps` | wait for healthy service or restart compose |
| port `31415` already used | `lsof -i :31415` | stop the other process or set `PROXIMA_MCP_BIND` |
| MCP auth rejected | bearer token mismatch | use `Authorization: Bearer pxm_<master-token>` for dev server |
| semantic search looks lexical | no embedding client configured or queue not drained | use lexical/hybrid search or configure embeddings per [docs/10-configuration.md](../10-configuration.md) |
| clippy fails on warnings | workspace denies warnings and pedantic lints | fix warning; do not allow/ignore unless design-approved |
| rustdoc fails | broken intra-doc link or warning | fix link/doc warning; CI uses `RUSTDOCFLAGS=-D warnings` |
