-- The stale-claim reclaim looks for `processing` jobs claimed long ago. Only
-- the jobs in flight are indexed, so the reclaim reads them and not the queue.
CREATE INDEX embedding_jobs_processing_idx
    ON proxima_core.embedding_jobs USING btree (claimed_at)
    WHERE status = 'processing'::proxima_core.embedding_job_status;
