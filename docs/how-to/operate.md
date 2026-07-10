# Operate (Day-2)

Runbook for running a deployed Code-flavor MCP server. Deployment and env
reference: [15-deployment.md](../15-deployment.md). Symptom lookup:
[troubleshoot.md](troubleshoot.md).

Readiness probe: read the `proxima://graph` MCP resource (`core/get_graph`).
There is no HTTP health endpoint — the process is ready when the resource
returns; auth/DB failures surface on that read.

## Backup and restore

Proxima owns exactly two durable stores; back up both:

| Store | What | Tooling |
|---|---|---|
| Postgres | graph, sidecars, goals, embeddings, jobs | standard `pg_dump` / `pg_basebackup` |
| S3 (optional) | content-addressed cited blobs | bucket versioning + lifecycle |

- PITR = standard Postgres WAL archiving + `pg_basebackup`; Proxima adds no
  backup verb and no app-level snapshot format.
- Restore order on a fresh DB: restore Postgres, then point the host at it. S3
  blobs are content-addressed, so a PG restore ahead of an S3 restore leaves
  citations that resolve once the bucket is back; no cross-store transaction.
- Embeddings are rebuildable rows, not source of truth: a lost embedding row
  re-enqueues (see the backlog signals below), so a PG-only restore is
  functionally complete for search once the drainer catches up.

## Failed migration on boot

Migrations run automatically on first boot and fail closed on the pgvector
version/GUC preflight (see [15 §Runtime requirements](../15-deployment.md#runtime-requirements)).

| Boot error | Meaning | Action |
|---|---|---|
| `ProximaError::V004ResetRequired` / `EmbedError::V004ResetRequired` | DB carries pre-v0.0.4 schema artifacts or a stale baseline checksum | export, then reset the DB per [MIGRATING.md](https://github.com/Aquilo-Solution-S/Proxima/blob/main/MIGRATING.md) — never in-place migrate |
| `EmbedError::Storage(String)` | generic connection / migration / preflight failure | retryable infra: fix connectivity or pgvector version, restart |

The reset variant is typed (distinct from the `Storage` string) precisely so a
host matches on it and prints `MIGRATING.md` guidance instead of parsing a
message.

## Signal → action

`proxima://graph` fields (owner-scoped, read on any authed session):

| Signal | Condition | Action |
|---|---|---|
| `embeddings_client_configured` | `false` | no embedding client; search is lexical-only (`degraded_to_lexical=true`). Set `MISTRAL_API_KEY` / `PROXIMA_EMBED_MODEL` if semantic recall is expected (see [10-configuration.md](../10-configuration.md)) |
| `pending_embedding_jobs` | `> 0`, trending down | normal in-process catch-up; no action |
| `pending_embedding_jobs` | `> 0`, flat/rising | drainer stalled or client unreachable; check embedding client reachability and logs |

`Engine::embedding_ann_observability(authz)` — host-only operator method
(`AuthPath::System` or `ComplianceAdminPort::may_perform_operator_maintenance`),
not an MCP tool (see [15 §Embedding Ops](../15-deployment.md#embedding-ops)):

| Signal | Condition | Action |
|---|---|---|
| `backlog.failed` | `> 0` | jobs erroring; inspect drainer logs (jobs retry) |
| `stale_processing_jobs` | `> 0` persistent | claims stuck past the stale-reclaim window (crashed drainer); auto-reclaimed, investigate if it does not clear |
| `orphan_rows.{embeddings,heads,jobs}` | `> 0` | crash-residue infra rows; run `Engine::sweep_orphan_embedding_rows` (same authz) |
| `recall_canary.recall_at_k` | low vs `k` | ANN recall degraded against exact; consider HNSW rebuild / `hnsw.ef_search` tuning (see [15 §Embedding Ops](../15-deployment.md#embedding-ops)) |

The orphan sweep's relationship to compliance erase is defined in
[15 §Embedding Ops](../15-deployment.md#embedding-ops).
