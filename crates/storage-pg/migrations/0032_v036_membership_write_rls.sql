-- v0.0.36: enforce subgroup write authority and scope lock-request reads.
-- Group edges require both endpoints for INSERT/UPDATE; DELETE retains
-- either endpoint's authority. Lock requests keep both trees serialized and
-- permit reads and writes when either endpoint is managed.
ALTER POLICY proxima_owner_write ON proxima_core.group_edges
    WITH CHECK (
        parent_group_id = ANY(COALESCE(NULLIF(current_setting('app.manage_owner', true), '')::uuid[], '{}'::uuid[]))
        AND child_group_id = ANY(COALESCE(NULLIF(current_setting('app.manage_owner', true), '')::uuid[], '{}'::uuid[]))
    );

ALTER POLICY proxima_owner_read ON proxima_core.group_lock_request
    USING (
        left_group_id = ANY(COALESCE(NULLIF(current_setting('app.manage_owner', true), '')::uuid[], '{}'::uuid[]))
        OR right_group_id = ANY(COALESCE(NULLIF(current_setting('app.manage_owner', true), '')::uuid[], '{}'::uuid[]))
    );
