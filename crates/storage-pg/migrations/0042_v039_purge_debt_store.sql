-- #482: purge debt names the object store that holds its bytes, not only
-- a bucket name. Two stores that share a bucket name (another endpoint,
-- another account) were indistinguishable, and the queue's key was the
-- object key alone, so one key could not be owed in two stores.
--
-- The identity is written by the store at prepare time and copied into
-- each debt. Existing rows have none to copy, so a store holding uploads
-- or debt refuses: this release already starts on a fresh store (0038).
SELECT set_config('app.proxima_scope', 'platform', true);
LOCK TABLE proxima_core.blob_uploads, proxima_core.cold_purge_pending IN SHARE MODE;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM proxima_core.blob_uploads)
       OR EXISTS (SELECT 1 FROM proxima_core.cold_purge_pending)
    THEN
        RAISE EXCEPTION
            'fresh store required: cited uploads or object purge debt exist'
            USING ERRCODE = '55000',
                  HINT = 'Create a fresh store before starting this release.';
    END IF;
END;
$$;

ALTER TABLE proxima_core.blob_uploads
    ADD COLUMN object_store text NOT NULL,
    ADD CONSTRAINT blob_uploads_object_store_chk
        CHECK (object_store ~ '^[a-z][a-z0-9]*:' AND octet_length(object_store) <= 512);

DROP INDEX proxima_core.cold_purge_pending_backend_idx;
ALTER TABLE proxima_core.cold_purge_pending
    DROP CONSTRAINT cold_purge_pending_pkey,
    DROP COLUMN backend,
    ADD COLUMN object_store text NOT NULL,
    ADD CONSTRAINT cold_purge_pending_object_store_chk
        CHECK (object_store ~ '^[a-z][a-z0-9]*:' AND octet_length(object_store) <= 512),
    ADD CONSTRAINT cold_purge_pending_pkey PRIMARY KEY (object_store, object_key);
CREATE INDEX cold_purge_pending_store_idx
    ON proxima_core.cold_purge_pending (object_store, enqueued_at, object_key);
