-- #514: a decompose child names its parent goal's series handle, so a
-- parent can be refused Achieved while a child is open. The parent is the
-- series key (goal.handle, stable across versions), not a version t: the
-- children of every parent version count. Each successor version of a
-- child copies the edge forward, and a trigger in the definitions keeps it
-- fixed for the life of a series.
--
-- No backfill and no fresh-store guard: 0038 already refuses a used store
-- in this unreleased cycle, so only goals that pre-release builds wrote
-- between 0038 and 0041 keep NULL, and those are not counted as children.
-- That affects dev stores only.
--
-- No foreign key: a source-scoped owner erase deletes a selected subset of
-- goal rows, and a dangling parent_handle after it does no harm.
SELECT set_config('app.proxima_scope', 'platform', true);

ALTER TABLE proxima_core.goal ADD COLUMN parent_handle uuid NULL;

ALTER TABLE proxima_core.goal
    ADD CONSTRAINT goal_parent_not_self_chk
    CHECK (parent_handle IS NULL OR parent_handle <> handle);

CREATE INDEX goal_parent_handle_idx
    ON proxima_core.goal (owner_id, parent_handle)
 WHERE parent_handle IS NOT NULL;
