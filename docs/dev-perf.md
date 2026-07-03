# Perf reducer fixtures

## Reducer smoke

`node scripts/perf-smoke.mjs` runs the summary reducer against committed
fixtures and compares to a golden `summary.expected.md`. Use `--regen`
after intentional reducer changes.

## ANN baseline

`python3 tools/ann-bench/ann_bench.py --database-url <url> --hot-rows 5000
--cold-rows 120 --noise-rows 15000 --queries-per-case 3` creates a disposable
`ann_bench` schema and writes `bench/ann-baseline.json`.

`python3 tools/ann-bench/ann_bench.py --database-url <url> --output
bench/ann-final.json --artifact-kind final --hot-rows 5000 --cold-rows 120
--noise-rows 15000 --queries-per-case 3 --ef-search 100 --iterative-scan
relaxed_order` writes the Plan-4 final comparison artifact.

The tracked artifacts use a production-style synthetic query shape:
`candidates -> eligible_entities -> vector_candidates` with the production
overfetch formula (`max(512, min(k, 50) * 64)`). They do not force HNSW.
`--force-hnsw` is probe-only and must not be used for tracked baseline / final
artifacts unless production search also forces that planner knob.

The baseline measures the current planner over the shared HNSW index, owner
btree index, and exact paths for unfiltered, owner-filtered hot, and
owner-filtered cold cases:

| field | meaning |
|---|---|
| `recall_at_k` | ANN result overlap with an exact seqscan result |
| `latency_ms` | database execution time from `EXPLAIN ANALYZE` |
| `storage.hnsw_index_bytes` | memory-residency proxy for the HNSW graph |
| `buffers` | shared hit/read block proxy from `EXPLAIN BUFFERS` |

The final artifact applies the runtime semantic-search settings:
`hnsw.ef_search = 100` and `hnsw.iterative_scan = relaxed_order`. On the
tracked synthetic production-shaped corpus, the planner chooses exact /
owner-index paths rather than HNSW, so the tracked comparison proves no recall
loss/regression under that query shape. Plan-4 accepts that exact/owner-index
behavior for the tracked corpus; it does not claim this artifact validates HNSW
index execution. Forced-HNSW probes remain local dossier evidence only.
Halfvec, partial indexes, and partitioning remain schema decisions outside this
change.

Current Plan-4 baseline vs final:

| case | baseline recall avg/min | final recall avg/min | baseline p95 ms | final p95 ms | planner |
|---|---:|---:|---:|---:|---|
| unfiltered | 1.0 / 1.0 | 1.0 / 1.0 | 48.7484 | 51.2997 | exact seqscan + sort |
| owner-filtered hot | 1.0 / 1.0 | 1.0 / 1.0 | 15.7117 | 16.0474 | owner bitmap + sort |
| owner-filtered cold | 1.0 / 1.0 | 1.0 / 1.0 | 0.5867 | 0.5893 | owner index / pkey + sort |

## ANN Ops

Owner-agnostic embedding ops signals are Host API only:
`Engine::embedding_ann_observability(authz)` requires `AuthPath::System` or
`ComplianceAdminPort::may_perform_operator_maintenance`.

| signal | source |
|---|---|
| embedding rows / head rows / job rows | `proxima_core.embeddings`, `embedding_heads`, `embedding_jobs` |
| HNSW bytes | `pg_relation_size('proxima_core.idx_embeddings_vec_hnsw'::regclass)` |
| total embedding relation bytes | `pg_total_relation_size('proxima_core.embeddings'::regclass)` |
| backlog | pending / processing / failed `embedding_jobs` |
| stale jobs | `processing` jobs older than the 15 minute visibility timeout |
| orphan rows | embedding infra without a matching `memories` / `goals` source row |
| recall canary | exact seqscan top-k vs HNSW top-k overlap for one current head |

Manual bloat/vacuum probe:

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

Maintenance rules:

| item | rule |
|---|---|
| lawful wipe | compliance erase synchronously deletes embeddings, heads, and jobs at transaction commit |
| orphan sweep | `Engine::sweep_orphan_embedding_rows(authz)` deletes crash-residue only; never part of compliance erase semantics |
| HNSW churn | vacuum is physical-index maintenance; schedule from dead tuples, bytes, latency, and recall canary |
| halfvec | rejected for this slice; final benchmark recovered filtered recall without schema/type migration |
