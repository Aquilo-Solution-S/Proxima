-- Writer attribution belongs to each immutable admission and durable grant.
SELECT set_config('app.proxima_scope', 'platform', true);
LOCK TABLE proxima_core.memory, proxima_core.cooled,
    proxima_core.goal, proxima_core.delegated_authority_grants IN SHARE MODE;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM proxima_core.memory)
       OR EXISTS (SELECT 1 FROM proxima_core.cooled)
       OR EXISTS (SELECT 1 FROM proxima_core.goal)
       OR EXISTS (SELECT 1 FROM proxima_core.delegated_authority_grants)
    THEN
        RAISE EXCEPTION
            'fresh store required: Memory, Goal, cooled memory, or delegated authority exists'
            USING ERRCODE = '55000',
                  HINT = 'Create a fresh store before starting this release.';
    END IF;
END;
$$;

ALTER TABLE proxima_core.memory
    ADD COLUMN "by" uuid NOT NULL,
    ADD COLUMN via uuid;
ALTER TABLE proxima_core.cooled
    ADD COLUMN "by" uuid NOT NULL,
    ADD COLUMN via uuid;
ALTER TABLE proxima_core.delegated_authority_grants
    ADD COLUMN "by" uuid NOT NULL,
    ADD COLUMN via uuid;
ALTER TABLE proxima_core.goal
    ADD COLUMN "by" uuid NOT NULL,
    ADD COLUMN via uuid;
