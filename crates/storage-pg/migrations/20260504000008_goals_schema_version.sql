-- M7 — make goal schema version queryable from the substrate row.
--
-- Mirrors memories.schema_version (M6). Goals carried no version on
-- the row; the writer-known version was only preserved in
-- change_event.entity_schema_version, so generic reads silently
-- collapsed to v1 regardless of what was written.

ALTER TABLE proxima_core.goals
    ADD COLUMN schema_version int;

UPDATE proxima_core.goals g
SET schema_version = ce.entity_schema_version
FROM proxima_core.change_event ce
WHERE ce.kind = 'EntityAppend'
  AND ce.entity_goal_id = g.goal_id
  AND g.schema_version IS NULL;

ALTER TABLE proxima_core.goals
    ALTER COLUMN schema_version SET NOT NULL;

ALTER TABLE proxima_core.goals
    ADD CONSTRAINT goals_schema_version_positive_chk
    CHECK (schema_version > 0);
