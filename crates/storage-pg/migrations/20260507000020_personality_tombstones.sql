-- Personality wake_config tombstones — operational config, not cognitive Memory.
-- Adds a tombstoned_at column and extends the status check to allow
-- 'tombstoned'. Existing dispatcher selection (status = 'active') is
-- unaffected; new SELECTs guard with `status <> 'tombstoned'`.

ALTER TABLE proxima_core.personality_wake_config
    ADD COLUMN IF NOT EXISTS tombstoned_at timestamptz;

ALTER TABLE proxima_core.personality_wake_config
    DROP CONSTRAINT IF EXISTS personality_wake_config_status_check;
ALTER TABLE proxima_core.personality_wake_config
    DROP CONSTRAINT IF EXISTS personality_wake_config_status_chk;
ALTER TABLE proxima_core.personality_wake_config
    DROP CONSTRAINT IF EXISTS personality_wake_config_tombstoned_at_chk;

ALTER TABLE proxima_core.personality_wake_config
    ADD CONSTRAINT personality_wake_config_status_chk
        CHECK (status IN ('active', 'needs_repair', 'tombstoned')),
    ADD CONSTRAINT personality_wake_config_tombstoned_at_chk
        CHECK (
            (status = 'tombstoned' AND tombstoned_at IS NOT NULL)
            OR (status <> 'tombstoned' AND tombstoned_at IS NULL)
        );
