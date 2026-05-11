ALTER TABLE proxima_core.personality_wake_entries
    DROP CONSTRAINT IF EXISTS personality_wake_entries_rounds_chk;

ALTER TABLE proxima_core.personality_wake_entries
    ADD CONSTRAINT personality_wake_entries_rounds_chk
        CHECK (max_rounds >= 0);
