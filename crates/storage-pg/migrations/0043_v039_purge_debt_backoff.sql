-- #482: a purge debt that keeps failing gives way to the rest of the queue.
-- Each failed delete records the attempt and backs the debt off; the drain
-- selects only debts that are due, so a failing key no longer holds back the
-- keys behind it. Attempts survive a re-enqueue, so a key that keeps failing
-- stays visible however often it is owed again.
--
-- No refusal: 0042 already required an empty queue, and the defaults below
-- make every existing row due now with no attempts.
SELECT set_config('app.proxima_scope', 'platform', true);

ALTER TABLE proxima_core.cold_purge_pending
    ADD COLUMN attempts integer NOT NULL DEFAULT 0,
    ADD COLUMN next_attempt_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN last_attempt_at timestamptz,
    ADD COLUMN last_error text,
    ADD CONSTRAINT cold_purge_pending_attempts_chk CHECK (attempts >= 0),
    ADD CONSTRAINT cold_purge_pending_last_error_chk
        CHECK (last_error IS NULL OR octet_length(last_error) <= 1024);

DROP INDEX proxima_core.cold_purge_pending_store_idx;
CREATE INDEX cold_purge_pending_due_idx
    ON proxima_core.cold_purge_pending (object_store, next_attempt_at, enqueued_at, object_key);
