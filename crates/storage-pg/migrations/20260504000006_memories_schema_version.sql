-- M6 — make memory schema version queryable from the substrate row.
--
-- Query returns MemoryRow::{schema_id, schema_version}. The version is
-- writer-known at insert time and must survive generic reads without
-- guessing from the latest registered schema or from sidecar naming.

ALTER TABLE proxima_core.memories
    ADD COLUMN schema_version int;

UPDATE proxima_core.memories m
SET schema_version = e.schema_version
FROM proxima_core.events e
WHERE m.event_id = e.event_id
  AND m.schema_version IS NULL;

UPDATE proxima_core.memories m
SET schema_version = ce.entity_schema_version
FROM proxima_core.change_event ce
WHERE ce.kind = 'EntityAppend'
  AND ce.entity_memory_id = m.memory_id
  AND m.schema_version IS NULL;

ALTER TABLE proxima_core.memories
    ALTER COLUMN schema_version SET NOT NULL;

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_schema_version_positive_chk
    CHECK (schema_version > 0);
