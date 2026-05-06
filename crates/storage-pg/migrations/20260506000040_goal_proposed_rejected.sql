ALTER TABLE proxima_core.goals
    ADD COLUMN IF NOT EXISTS payload bytea NOT NULL DEFAULT ''::bytea;

ALTER TABLE proxima_core.goals
    DROP CONSTRAINT IF EXISTS goals_state_chk;

ALTER TABLE proxima_core.goals
    ADD CONSTRAINT goals_state_chk
    CHECK (state IN ('Proposed', 'Active', 'Paused', 'Achieved', 'Abandoned', 'Rejected'));

CREATE INDEX IF NOT EXISTS goals_proposed_inbox_idx
    ON proxima_core.goals (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        created_at DESC
    )
    WHERE state = 'Proposed';

CREATE OR REPLACE FUNCTION proxima_core.goals_pair_allowed(
    prior_state text,
    next_state text,
    authorship_kind text
) RETURNS boolean LANGUAGE sql IMMUTABLE AS $$
    SELECT (prior_state, next_state, authorship_kind) IN (
        ('Proposed', 'Active', 'User'),
        ('Proposed', 'Rejected', 'User'),
        ('Active', 'Active', 'User'),
        ('Active', 'Paused', 'User'),
        ('Active', 'Achieved', 'User'),
        ('Active', 'Achieved', 'System'),
        ('Active', 'Abandoned', 'User'),
        ('Paused', 'Active', 'User'),
        ('Paused', 'Abandoned', 'User')
    );
$$;

CREATE OR REPLACE FUNCTION proxima_core.goals_validate_transition()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    prior_state text;
BEGIN
    IF NEW.supersedes IS NULL THEN
        IF NEW.state = 'Rejected' THEN
            RAISE EXCEPTION 'goal: cannot create directly with state=Rejected';
        END IF;
        IF NEW.state IN ('Active', 'Paused', 'Achieved', 'Abandoned')
           AND NEW.authorship_kind NOT IN ('User', 'System') THEN
            RAISE EXCEPTION 'goal: only User/System may seed state=%', NEW.state;
        END IF;
        RETURN NEW;
    END IF;

    SELECT state INTO prior_state
      FROM proxima_core.goals
     WHERE goal_id = NEW.supersedes;

    IF prior_state IS NULL THEN
        RAISE EXCEPTION 'goal: supersedes references unknown id';
    END IF;
    IF prior_state IN ('Achieved', 'Abandoned', 'Rejected') THEN
        RAISE EXCEPTION 'goal: state=% is terminal', prior_state;
    END IF;
    IF NOT proxima_core.goals_pair_allowed(prior_state, NEW.state, NEW.authorship_kind) THEN
        RAISE EXCEPTION 'goal: forbidden transition %->% under authorship=%',
            prior_state, NEW.state, NEW.authorship_kind;
    END IF;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS goals_transition_check ON proxima_core.goals;

CREATE TRIGGER goals_transition_check
    BEFORE INSERT ON proxima_core.goals
    FOR EACH ROW EXECUTE FUNCTION proxima_core.goals_validate_transition();
