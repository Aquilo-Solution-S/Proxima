-- Proxima core schema — v0.0.8 draft migration (version 18): arm-matched
-- partial indexes for the embedding-job claim (sql-sweep finding S2).
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). This file is a v0.0.8-cycle DRAFT (docs/how-to/migrations.md):
-- squash at release preparation under a fresh version number, together
-- with 0016 and 0017. Same non-concurrent build caveat as 0017.

-- ---------------------------------------------------------------------------
-- The claim CTE orders by (enqueued_at, owner_kind, owner_id, entity_kind,
-- entity_id, embedding_version) under model_id equality, but the only index
-- (idx_embedding_jobs_status_enqueued: status, enqueued_at) lacks model_id
-- and the two-arm status OR defeats ordered index use, so every claim sorts
-- the whole claimable backlog (measured in the sweep, proxima-bench
-- docs/sql-sweep-findings.md S2: 3,090 buffers/37.6ms for ONE claimed job on
-- a 200k-row queue; with these two arm-matched partial indexes plus the
-- UNION ALL claim rewrite in fact_embeddings/jobs.rs: 6 buffers/0.040ms).
-- One partial index per status arm, matching that arm's predicate
-- (PostgreSQL docs §11.8 Partial Indexes, §13.3.3 FOR UPDATE SKIP LOCKED
-- queue pattern). Only the pending index also carries the claim's full
-- ORDER BY (enqueued_at, owner_kind, ...), so only the pending scan needs
-- no sort. The reclaim index is (model_id, updated_at): the staleness
-- cutoff becomes an index range condition, but the reclaim arm still
-- orders by enqueued_at, so its top-N sorts whatever stale-processing
-- rows the index finds (normally few). Whether the reclaim index should
-- instead carry the ORDER BY columns and leave updated_at residual is an
-- open design question journaled in docs/wave2-adjudications.md.
--
-- The pending arm's backoff gate (next_attempt_at IS NULL OR <= now()) and
-- the reclaim arm's staleness gate (updated_at < now() - 15min) stay
-- residual filters: both compare against now(), which an index predicate
-- cannot hold.
-- ---------------------------------------------------------------------------
CREATE INDEX idx_embedding_jobs_pending_claim
    ON proxima_core.embedding_jobs
       (model_id, enqueued_at, owner_kind, owner_id,
        entity_kind, entity_id, embedding_version)
    WHERE status = 'pending';

CREATE INDEX idx_embedding_jobs_processing_reclaim
    ON proxima_core.embedding_jobs (model_id, updated_at)
    WHERE status = 'processing';
