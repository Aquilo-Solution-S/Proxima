-- Schema evolution completion and serving write fence.
--
-- A completed source schema is no longer writable.  The migration executor
-- takes the exclusive transaction advisory lock for the same schema before it
-- transforms rows and records completion.  Ordinary writers take the shared
-- lock in the generated trigger and refuse the source after completion.

CREATE TABLE proxiima_core.schema_evolution (
    source_kind text NOT NULL CHECK (source_kind IN ('fact', 'abstraction', 'perspective', 'goal', 'cited_object')),
    source_schema_id text NOT NULL,
    evolution_id text NOT NULL,
    spiice_id text NOT NULL,
    target_schema_ids text[] NOT NULL,
    target_kinds text[] NOT NULL,
    completed_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT schema_evolution_source_nonblank_chk
        CHECK (length(btrim(source_schema_id)) > 0),
    CONSTRAINT schema_evolution_evolution_nonblank_chk
        CHECK (length(btrim(evolution_id)) > 0),
    CONSTRAINT schema_evolution_spiice_nonblank_chk
        CHECK (length(btrim(spiice_id)) > 0),
    CONSTRAINT schema_evolution_targets_nonempty_chk
        CHECK (cardinality(target_schema_ids) > 0 AND cardinality(target_kinds) = cardinality(target_schema_ids)),
    CONSTRAINT schema_evolution_targets_no_null_chk
        CHECK (array_position(target_schema_ids, NULL::text) IS NULL AND array_position(target_kinds, NULL::text) IS NULL),
    CONSTRAINT schema_evolution_targets_nonblank_chk
        CHECK (array_position(target_schema_ids, '') IS NULL AND array_position(target_kinds, '') IS NULL),
    CONSTRAINT schema_evolution_target_kinds_chk
        CHECK (target_kinds <@ ARRAY['fact', 'abstraction', 'perspective', 'goal', 'cited_object']::text[]),
    PRIMARY KEY (source_kind, source_schema_id)
);

CREATE INDEX schema_evolution_spiice_idx
    ON proxiima_core.schema_evolution (spiice_id, source_schema_id);

ALTER TABLE proxiima_core.goal_replay_declaration
    ADD COLUMN owner_id uuid,
    ADD COLUMN request_id text;
-- The pre-0065 database already has append-only UPDATE triggers installed.
-- Drop only these two guards for the metadata backfill, then restore their
-- ordinary behavior before this migration commits; the definitions pass later
-- replaces them with the migration-aware narrow guards.
DROP TRIGGER IF EXISTS goal_replay_declaration_append_only ON proxiima_core.goal_replay_declaration;
DROP TRIGGER IF EXISTS goal_append_only ON proxiima_core.goal;
UPDATE proxiima_core.goal_replay_declaration declaration
   SET owner_id = goal.owner_id,
       request_id = goal.request_id
  FROM proxiima_core.goal goal
 WHERE goal.t = declaration.goal_t;
ALTER TABLE proxiima_core.goal
    ADD CONSTRAINT goal_t_owner_key UNIQUE (t, owner_id);
ALTER TABLE proxiima_core.goal_replay_declaration
    ALTER COLUMN owner_id SET NOT NULL,
    ALTER COLUMN request_id SET NOT NULL,
    ADD CONSTRAINT goal_replay_declaration_goal_owner_fkey
        FOREIGN KEY (goal_t, owner_id)
        REFERENCES proxiima_core.goal(t, owner_id) ON DELETE CASCADE;
ALTER TABLE proxiima_core.goal_replay_declaration
    DROP CONSTRAINT goal_replay_declaration_pkey,
    ADD CONSTRAINT goal_replay_declaration_pkey PRIMARY KEY (owner_id, request_id);
CREATE INDEX goal_replay_declaration_goal_t_idx
    ON proxiima_core.goal_replay_declaration (goal_t);

ALTER TABLE proxiima_core.goal ADD COLUMN schema_id text;
UPDATE proxiima_core.goal goal
   SET schema_id = head.schema_id
  FROM proxiima_core.goal_head head
 WHERE head.handle = goal.handle;
ALTER TABLE proxiima_core.goal
    ALTER COLUMN schema_id SET NOT NULL;


CREATE TRIGGER goal_replay_declaration_append_only
    BEFORE UPDATE ON proxiima_core.goal_replay_declaration
    FOR EACH ROW EXECUTE FUNCTION proxiima_core.enforce_row_append_only();
CREATE TRIGGER goal_append_only
    BEFORE UPDATE ON proxiima_core.goal
    FOR EACH ROW EXECUTE FUNCTION proxiima_core.enforce_row_append_only();

CREATE TABLE proxiima_core.schema_evolution_authority (
    xact xid8 NOT NULL DEFAULT pg_current_xact_id(),
    record_kind text NOT NULL CHECK (record_kind IN ('memory', 'goal', 'blob')),
    source_id uuid NOT NULL,
    target_id uuid NOT NULL,
    source_schema text NOT NULL,
    target_schema text NOT NULL,
    source_handle uuid,
    target_handle uuid,
    record_owner uuid NOT NULL REFERENCES proxiima_core.owners(owner_id),
    primary_target boolean NOT NULL,
    source_pins uuid[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (xact, record_kind, source_id, source_schema, target_id)
);

CREATE TABLE proxiima_core.schema_evolution_publication (
    publication_t uuid NOT NULL REFERENCES proxiima_core.publication_origin(t) ON DELETE CASCADE,
    memory_t uuid NOT NULL REFERENCES proxiima_core.memory(t) ON DELETE CASCADE,
    PRIMARY KEY (publication_t, memory_t)
);
CREATE INDEX schema_evolution_publication_memory_idx
    ON proxiima_core.schema_evolution_publication (memory_t, publication_t);
