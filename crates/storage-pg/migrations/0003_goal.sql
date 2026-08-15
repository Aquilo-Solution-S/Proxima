-- Slice 4: Goal timeseries + write-act is a Fact (no extra table).

CREATE TYPE proxima_core.goal_state AS ENUM (
    'Active',
    'Paused',
    'Achieved',
    'Abandoned'
);

CREATE TABLE proxima_core.goal_head (
    handle uuid PRIMARY KEY,
    schema_id text NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    t uuid NOT NULL
);

CREATE INDEX goal_head_owner_schema_idx
    ON proxima_core.goal_head (owner_id, schema_id, handle);

CREATE TABLE proxima_core.goal (
    handle uuid NOT NULL REFERENCES proxima_core.goal_head (handle),
    t uuid NOT NULL DEFAULT uuidv7(),
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    title text NOT NULL,
    state proxima_core.goal_state NOT NULL,
    request_id text NOT NULL,
    close_fact_t uuid,
    assignment_t uuid,
    dependency_t uuid[] NOT NULL DEFAULT '{}',
    evidence_t uuid[] NOT NULL DEFAULT '{}',
    wake_id uuid,
    write_act_t uuid,
    PRIMARY KEY (handle, t),
    UNIQUE (t),
    UNIQUE (owner_id, request_id),
    CONSTRAINT goal_title_nonblank_chk CHECK (length(btrim(title)) > 0),
    CONSTRAINT goal_terminal_close_chk CHECK (
        (state IN ('Achieved', 'Abandoned')) = (close_fact_t IS NOT NULL)
    ),
    CONSTRAINT goal_arrays_no_null_chk CHECK (
        array_position(dependency_t, NULL) IS NULL
        AND array_position(evidence_t, NULL) IS NULL
    )
);

CREATE INDEX goal_owner_handle_t_idx
    ON proxima_core.goal (owner_id, handle, t DESC);

CREATE INDEX goal_wake_idx
    ON proxima_core.goal (wake_id) WHERE wake_id IS NOT NULL;

CREATE INDEX goal_dependency_gin
    ON proxima_core.goal USING gin (dependency_t);

CREATE INDEX goal_evidence_gin
    ON proxima_core.goal USING gin (evidence_t);

CREATE TRIGGER goal_append_only
    BEFORE UPDATE ON proxima_core.goal
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

CREATE FUNCTION proxima_core.goal_head_t_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
       OR NEW.owner_id IS DISTINCT FROM OLD.owner_id THEN
        RAISE EXCEPTION 'goal_head is frozen except t' USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER goal_head_t_only
    BEFORE UPDATE ON proxima_core.goal_head
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.goal_head_t_only();

CREATE FUNCTION proxima_core.goal_no_later_after_terminal() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM proxima_core.goal g
         WHERE g.handle = NEW.handle
           AND g.state IN ('Achieved', 'Abandoned')
           AND g.t <> NEW.t
    ) THEN
        RAISE EXCEPTION 'terminal goal admits no later t' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER goal_no_later_after_terminal
    BEFORE INSERT ON proxima_core.goal
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.goal_no_later_after_terminal();
