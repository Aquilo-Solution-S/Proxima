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
- **Verify the two stores agree after any restore that touched one of them**,
  with `proxima-mcp maintain-blobs`. A row whose object never came back is not
  self-correcting: the upload lane skips artefacts the corpus already claims to
  hold, so re-ingesting the same document will NOT replace the missing bytes,
  and the citation stays unresolvable with no error anywhere. `missing=` is the
  number that matters; `orphans=` is only cost.
  The command obtains global authority by booting the normal headless Proxima
  composition; it remains report-only and never deletes or repairs either
  store.
- Embeddings are rebuildable rows, not source of truth: a lost embedding row
  re-enqueues (see the backlog signals below), so a PG-only restore is
  functionally complete for search once the drainer catches up.

## Failed migration on boot

Migrations run automatically on first boot and fail closed on the pgvector
version/GUC preflight (see [15 §Runtime requirements](../15-deployment.md#runtime-requirements)).

| Boot error | Meaning | Action |
|---|---|---|
| `ProximaError::V004ResetRequired` / `EmbedError::V004ResetRequired` | DB carries pre-v0.0.4 schema artifacts or a stale baseline checksum | export, then reset the DB per [MIGRATING.md](https://github.com/Aquilo-Solution-S/Proxima/blob/main/MIGRATING.md) — never in-place migrate |
| `EmbedError::Storage(String)` containing `VersionMismatch` | a migration file changed after it was applied — its checksum no longer matches the `_sqlx_migrations` ledger | **not** retryable; restarting crashloops. Only reachable on a database that ran a pre-release build of a migration that was later edited. Repair the ledger: delete rows for versions that no longer exist as files, and reset the checksum for the edited version to the SHA-384 of the current file |
| `EmbedError::Storage(String)` containing `missing schema markers` | the database is a release lane behind the binary | apply the lane's migrations from [MIGRATING.md](https://github.com/Aquilo-Solution-S/Proxima/blob/main/MIGRATING.md), then restart |
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

Vacuum is physical-index maintenance; schedule from dead tuples, bytes,
latency, and the recall canary.

```sql
SELECT relname, n_live_tup, n_dead_tup, vacuum_count, autovacuum_count
  FROM pg_stat_all_tables
 WHERE schemaname = 'proxima_core'
   AND relname IN ('embeddings', 'embedding_heads', 'embedding_jobs');

SELECT
    pg_relation_size('proxima_core.embeddings'::regclass) AS embedding_table_bytes,
    pg_total_relation_size('proxima_core.embeddings'::regclass) AS embedding_total_bytes,
    pg_relation_size('proxima_core.idx_embeddings_vec_hnsw'::regclass) AS hnsw_index_bytes;
```
