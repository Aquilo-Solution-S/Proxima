-- #482: an erase owes every key an upload implies and asks, per key,
-- whether a surviving row implies it too, including a row that mounts the
-- object. That arm looks rows up by mounted_from_upload_id; index it so the
-- refcount stays an index probe per key.
SELECT set_config('app.proxima_scope', 'platform', true);

CREATE INDEX blob_uploads_mounted_from_idx
    ON proxima_core.blob_uploads (mounted_from_upload_id)
    WHERE mounted_from_upload_id IS NOT NULL;
