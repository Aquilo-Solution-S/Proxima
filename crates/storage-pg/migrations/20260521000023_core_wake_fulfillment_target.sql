ALTER TABLE proxima_core.personality_wake_entries
    ADD COLUMN required_produced_schema_ids text[] NOT NULL DEFAULT '{}';
