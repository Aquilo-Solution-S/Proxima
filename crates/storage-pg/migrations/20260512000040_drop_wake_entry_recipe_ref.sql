DO $$
BEGIN
    EXECUTE 'ALTER TABLE proxima_core.personality_wake_entries DROP COLUMN IF EXISTS recipe' || '_ref';
END
$$;
