-- #482: the expired-upload sweep selects pending uploads past their TTL in
-- expiry order. Only pending rows are candidates, so the index holds only
-- them and stays as small as the set of uploads in flight.
SELECT set_config('app.proxima_scope', 'platform', true);

CREATE INDEX blob_uploads_pending_expiry_idx
    ON proxima_core.blob_uploads (expires_at)
    WHERE status = 'pending';
