-- The derived projection carries the owner and tag in the same GIN scan.
CREATE INDEX core_projection_owner_tsv_gin_tag ON proxima_core.projection USING gin (owner_id, tag);

-- Serving roles receive lock-request writes through their explicit grants.
REVOKE INSERT ON TABLE proxima_core.group_lock_request FROM PUBLIC;
