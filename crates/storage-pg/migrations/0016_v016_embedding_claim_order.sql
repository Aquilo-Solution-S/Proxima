-- v0.0.16, second core file: the embedding drain claims across every space.
--
-- Per-Owner embedding routing embeds each job through its Owner's route, so
-- the drainer claims in queue order across every `(model_id, dim)` space
-- instead of one space at a time. 0015 keyed the pending-claim index on the
-- space first; it merged before routing did and may already be applied, so
-- its bytes stay and this re-keys the index on queue position alone.
--
-- Cost: the index is partial on `status = 'pending'`, so the rebuild reads
-- only the live backlog.

DROP INDEX proxima_core.embedding_jobs_pending_claim_idx;
CREATE INDEX embedding_jobs_pending_claim_idx
    ON proxima_core.embedding_jobs (job_id)
    WHERE status = 'pending';
