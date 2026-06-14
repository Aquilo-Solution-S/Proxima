ALTER TABLE proxima_core.memories
    ADD COLUMN tombstoned_at timestamptz NULL;

ALTER TYPE proxima_core.change_event_kind ADD VALUE 'EntityDelete';

ALTER TABLE ONLY proxima_core.change_event
    DROP CONSTRAINT change_event_entity_memory_id_fkey;
