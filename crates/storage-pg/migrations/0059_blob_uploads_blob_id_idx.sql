-- The uploads of a blob, by blob. `blob_uploads.blob_id` references `blob`,
-- and Postgres indexes no referencing column of its own: without this index
-- every delete of a blob row scans all `blob_uploads` of the store for the
-- foreign-key check, and so does each read of a blob's uploads (the erase
-- fence keys and the unreferenced-blob sweep). The work then grows with the
-- store's uploads, not with the blobs erased. Partial like
-- `memory_blob_id_idx`: a pending or aborted upload has no blob.
CREATE INDEX blob_uploads_blob_id_idx ON proxima_core.blob_uploads USING btree (blob_id) WHERE (blob_id IS NOT NULL);
