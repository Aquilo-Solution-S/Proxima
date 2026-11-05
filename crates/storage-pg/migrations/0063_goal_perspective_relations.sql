-- Goal/Perspective causal relations.
--
-- These arrays hold exact version ids and are source-owned. They are separate
-- from generic payload references (`memory.goal_refs`) and do not contribute
-- F/A/P grounding. New writes validate kind and owner in the storage verbs
-- and the definitions below.

ALTER TABLE proxiima_core.goal
    ADD COLUMN motivated_by uuid[] DEFAULT '{}'::uuid[] NOT NULL;

ALTER TABLE proxiima_core.memory
    ADD COLUMN guided_by uuid[] DEFAULT '{}'::uuid[] NOT NULL;

ALTER TABLE proxiima_core.goal
    ADD CONSTRAINT goal_motivated_by_no_null_chk
    CHECK (array_position(motivated_by, NULL::uuid) IS NULL);

ALTER TABLE proxiima_core.memory
    ADD CONSTRAINT memory_guided_by_no_null_chk
    CHECK (array_position(guided_by, NULL::uuid) IS NULL);

CREATE INDEX goal_motivated_by_gin
    ON proxiima_core.goal USING gin (motivated_by);

CREATE INDEX memory_guided_by_gin
    ON proxiima_core.memory USING gin (guided_by);

-- Recall selects either contextual motivations or owner-scoped Goals without
-- motivation. Keep both access paths selective under owner RLS.
CREATE INDEX goal_unmotivated_owner_state_idx
    ON proxiima_core.goal (owner_id, state, handle DESC, t DESC)
    WHERE motivated_by = '{}'::uuid[];

DROP INDEX proxiima_core.goal_assignment_state_t_idx;
DROP INDEX proxiima_core.goal_unassigned_owner_state_idx;

-- Canonical Goal motivation remains the source-owned array above. This scalar
-- projection is maintained by triggers solely for selective reverse reads;
-- callers never write it directly.
CREATE TABLE proxiima_core.goal_motivation_index (
    goal_t uuid NOT NULL REFERENCES proxiima_core.goal(t) ON DELETE CASCADE,
    perspective_t uuid NOT NULL,
    owner_id uuid NOT NULL REFERENCES proxiima_core.owners(owner_id),
    PRIMARY KEY (goal_t, perspective_t)
);

CREATE INDEX goal_motivation_index_owner_perspective
    ON proxiima_core.goal_motivation_index (owner_id, perspective_t, goal_t);

CREATE OR REPLACE FUNCTION proxiima_core.sync_goal_motivation_index()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    DELETE FROM proxiima_core.goal_motivation_index WHERE goal_t = NEW.t;
    INSERT INTO proxiima_core.goal_motivation_index (goal_t, perspective_t, owner_id)
    SELECT NEW.t, p.id, NEW.owner_id
      FROM unnest(NEW.motivated_by) AS p(id);
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$function$;
REVOKE ALL ON FUNCTION proxiima_core.sync_goal_motivation_index() FROM PUBLIC;

CREATE TRIGGER goal_motivation_index_sync
AFTER INSERT ON proxiima_core.goal
FOR EACH ROW EXECUTE FUNCTION proxiima_core.sync_goal_motivation_index();

CREATE TRIGGER goal_motivation_index_sync_update
AFTER UPDATE OF motivated_by ON proxiima_core.goal
FOR EACH ROW EXECUTE FUNCTION proxiima_core.sync_goal_motivation_index();

INSERT INTO proxiima_core.goal_motivation_index (goal_t, perspective_t, owner_id)
SELECT g.t, p.id, g.owner_id
  FROM proxiima_core.goal AS g
 CROSS JOIN LATERAL unnest(g.motivated_by) AS p(id);

ALTER TABLE proxiima_core.goal_motivation_index ENABLE ROW LEVEL SECURITY;
ALTER TABLE ONLY proxiima_core.goal_motivation_index FORCE ROW LEVEL SECURITY;
