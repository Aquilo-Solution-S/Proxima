-- v0.0.15 owner-RLS cutover. Stop every older pack before applying.
-- Run as the nonsuperuser platform owner; older binaries cannot resume afterward.

DO $owner_rls_role$
DECLARE
    role_is_superuser boolean;
    role_bypasses_rls boolean;
BEGIN
    SELECT r.rolsuper, r.rolbypassrls
      INTO role_is_superuser, role_bypasses_rls
      FROM pg_roles AS r
     WHERE r.rolname = current_user;
    IF role_is_superuser OR role_bypasses_rls THEN
        RAISE EXCEPTION 'owner RLS migration role must be NOSUPERUSER NOBYPASSRLS';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM pg_class AS c
          JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = 'proxima_core'
           AND c.relkind IN ('r', 'p')
           AND NOT pg_has_role(current_user, c.relowner, 'USAGE')
    ) THEN
        RAISE EXCEPTION 'owner RLS migration role does not own every composed table';
    END IF;
END
$owner_rls_role$;

-- Ordered owner streams let head pagination stop after the visible page.
-- Keep both shapes: a schema prefix cannot order a schema-free owner read.
CREATE INDEX IF NOT EXISTS memory_head_owner_t_idx
    ON proxima_core.memory_head (owner_id, t DESC) INCLUDE (handle, kind);
CREATE INDEX IF NOT EXISTS memory_head_owner_schema_t_idx
    ON proxima_core.memory_head (owner_id, schema_id, t DESC) INCLUDE (handle, kind);

CREATE TABLE IF NOT EXISTS proxima_core.owner_rls_epoch (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    epoch integer NOT NULL
);

INSERT INTO proxima_core.owner_rls_epoch (singleton, epoch)
VALUES (true, 1)
ON CONFLICT (singleton) DO UPDATE SET epoch = EXCLUDED.epoch;

DO $owner_rls$
DECLARE
    relation record;
    fk record;
    parent_has_t boolean;
    owner_role text;
    read_expr text;
    write_expr text;
    ownerless boolean;
    relation_key text;
    approved_fk_tables constant text[] := ARRAY[
        'proxima_core.agent_derivation_v1', 'proxima_core.agent_note_v1',
        'proxima_core.interpretation_v1', 'proxima_core.mcp_call_logged_v1',
        'proxima_core.task_goal_v1', 'proxima_core.utterance_v1',
        'proxima_core.write_act_v1', 'proxima_core.goal_replay_declaration'
    ];
    metadata_tables constant text[] := ARRAY[
        'proxima_core.flavor_surface', 'proxima_core.lexical_default',
        'proxima_core.lexical_languages'
    ];
    ownerless_tables constant text[] := ARRAY[
        'proxima_core.erased_pin_target',
        'proxima_core.flavor_surface',
        'proxima_core.group_memberships',
        'proxima_core.installation',
        'proxima_core.lexical_default',
        'proxima_core.lexical_languages',
        'proxima_core.owner_rls_epoch'
    ];
BEGIN
    FOR relation IN
        SELECT c.oid,
               n.nspname AS schema_name,
               c.relname,
               c.relowner,
               EXISTS (
                   SELECT 1
                     FROM pg_attribute AS a
                    WHERE a.attrelid = c.oid
                      AND a.attname = 'owner_id'
                      AND NOT a.attisdropped
                      AND a.attnum > 0
               ) AS has_owner_id
          FROM pg_class AS c
          JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = 'proxima_core'
           AND c.relkind IN ('r', 'p')
         ORDER BY n.nspname, c.relname
    LOOP
        owner_role := pg_get_userbyid(relation.relowner);
        relation_key := format('%I.%I', relation.schema_name, relation.relname);
        ownerless := relation_key = ANY(ownerless_tables);

        IF relation_key = ANY(metadata_tables) THEN
            read_expr := $expr$current_setting('app.proxima_scope', true) = 'owner' AND cardinality(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[])) > 0$expr$;
            write_expr := $expr$current_setting('app.proxima_scope', true) = 'owner' AND cardinality(COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[])) > 0$expr$;
        ELSIF relation.schema_name = 'proxima_core' AND relation.relname = 'group_memberships' THEN
            read_expr := $expr$group_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
            write_expr := $expr$group_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.manage_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
        ELSIF relation.schema_name = 'proxima_core' AND relation.relname = 'closed_handle' THEN
            read_expr := $expr$EXISTS (SELECT 1 FROM proxima_core.memory_head AS parent WHERE parent.handle = proxima_core.closed_handle.handle AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[]))$expr$;
            write_expr := $expr$EXISTS (SELECT 1 FROM proxima_core.memory_head AS parent WHERE parent.handle = proxima_core.closed_handle.handle AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[]))$expr$;
        ELSIF relation.schema_name = 'proxima_core' AND relation.relname = 'publication_origin' THEN
            read_expr := $expr$original_owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
            write_expr := $expr$original_owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
        ELSIF relation.has_owner_id THEN
            IF relation.schema_name = 'proxima_core' AND relation.relname = 'memory' THEN
                read_expr := $expr$owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])
                    AND ((kind = 'fact' AND owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[]))
                      OR (kind = 'abstraction' AND owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.read_abstraction', true), '')::uuid[], '{}'::uuid[]))::uuid[]))
                      OR (kind = 'perspective' AND owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.read_perspective', true), '')::uuid[], '{}'::uuid[]))::uuid[])))$expr$;
                write_expr := $expr$owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])
                    AND ((kind = 'fact' AND owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[]))
                      OR (kind = 'abstraction' AND owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_abstraction', true), '')::uuid[], '{}'::uuid[]))::uuid[]))
                      OR (kind = 'perspective' AND owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_perspective', true), '')::uuid[], '{}'::uuid[]))::uuid[])))$expr$;
            ELSIF relation.schema_name = 'proxima_core' AND relation.relname = 'goal' THEN
                read_expr := $expr$owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.read_goal', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
                write_expr := $expr$owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_goal', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
            ELSE
                read_expr := $expr$owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
                write_expr := $expr$owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
            END IF;
        ELSIF ownerless THEN
            -- Explicitly classified metadata and bootstrap tables are denied
            -- to owner scope.  Their maintenance path is platform-only.
            read_expr := 'false';
            write_expr := 'false';
        ELSE
            IF relation_key <> ALL(approved_fk_tables) THEN
                RAISE EXCEPTION 'owner RLS classification missing for %.%', relation.schema_name, relation.relname;
            END IF;
            SELECT child_att.attname AS child_column,
                   parent_ns.nspname AS parent_schema,
                   parent.relname AS parent_table,
                   parent_att.attname AS parent_column
              INTO fk
              FROM pg_constraint AS con
              JOIN pg_attribute AS child_att
                ON child_att.attrelid = relation.oid AND child_att.attnum = con.conkey[1]
              JOIN pg_class AS parent ON parent.oid = con.confrelid
              JOIN pg_namespace AS parent_ns ON parent_ns.oid = parent.relnamespace
              JOIN pg_attribute AS parent_att
                ON parent_att.attrelid = parent.oid AND parent_att.attnum = con.confkey[1]
             WHERE con.conrelid = relation.oid
               AND con.contype = 'f'
               AND array_length(con.conkey, 1) = 1
               AND array_length(con.confkey, 1) = 1
               AND parent_ns.nspname = 'proxima_core'
             ORDER BY (parent.relname IN ('memory', 'goal')) DESC, con.oid
             LIMIT 1;

            IF fk IS NULL THEN
                RAISE EXCEPTION 'owner RLS classification missing for %.%', relation.schema_name, relation.relname;
            END IF;
            SELECT EXISTS (
                SELECT 1 FROM pg_attribute
                 WHERE attrelid = format('%I.%I', fk.parent_schema, fk.parent_table)::regclass
                   AND attname = 't' AND attnum > 0 AND NOT attisdropped
            ) INTO parent_has_t;
            read_expr := format(
                'EXISTS (SELECT 1 FROM %I.%I AS parent WHERE parent.%I = %I.%I.%I AND current_setting(''app.proxima_scope'', true) = ''owner'')',
                fk.parent_schema, fk.parent_table, fk.parent_column, relation.schema_name, relation.relname, fk.child_column);
            IF fk.parent_table = 'memory' THEN
                write_expr := format(
                    'EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.%I = %I.%I.%I AND ((parent.kind = ''fact'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''abstraction'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''perspective'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))))',
                    fk.parent_column, relation.schema_name, relation.relname, fk.child_column);
            ELSIF fk.parent_table = 'goal' THEN
                write_expr := format(
                    'EXISTS (SELECT 1 FROM proxima_core.goal AS parent WHERE parent.%I = %I.%I.%I AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_goal'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))',
                    fk.parent_column, relation.schema_name, relation.relname, fk.child_column);
            ELSIF parent_has_t THEN
                write_expr := format(
                    'EXISTS (SELECT 1 FROM %I.%I AS parent JOIN proxima_core.memory AS owner_memory ON owner_memory.t = parent.t WHERE parent.%I = %I.%I.%I AND ((owner_memory.kind = ''fact'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (owner_memory.kind = ''abstraction'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (owner_memory.kind = ''perspective'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))))',
                    fk.parent_schema, fk.parent_table, fk.parent_column, relation.schema_name, relation.relname, fk.child_column);
            ELSE
                RAISE EXCEPTION 'owner RLS writable classification missing for %.%', relation.schema_name, relation.relname;
            END IF;
        END IF;

        EXECUTE format('ALTER TABLE %I.%I ENABLE ROW LEVEL SECURITY', relation.schema_name, relation.relname);
        EXECUTE format('ALTER TABLE %I.%I FORCE ROW LEVEL SECURITY', relation.schema_name, relation.relname);
        EXECUTE format('DROP POLICY IF EXISTS proxima_owner_read ON %I.%I', relation.schema_name, relation.relname);
        EXECUTE format('DROP POLICY IF EXISTS proxima_owner_write ON %I.%I', relation.schema_name, relation.relname);
        EXECUTE format('DROP POLICY IF EXISTS proxima_platform ON %I.%I', relation.schema_name, relation.relname);

        EXECUTE format(
            'CREATE POLICY proxima_owner_read ON %I.%I FOR SELECT TO PUBLIC USING (%s)',
            relation.schema_name, relation.relname, read_expr);
        EXECUTE format(
            'CREATE POLICY proxima_owner_write ON %I.%I FOR ALL TO PUBLIC USING (%s) WITH CHECK (%s)',
            relation.schema_name, relation.relname, write_expr, write_expr);
        EXECUTE format(
            'CREATE POLICY proxima_platform ON %I.%I FOR ALL TO %I USING (current_setting(''app.proxima_scope'', true) = ''platform'') WITH CHECK (current_setting(''app.proxima_scope'', true) = ''platform'')',
            relation.schema_name, relation.relname, owner_role);
    END LOOP;
END
$owner_rls$;

COMMENT ON TABLE proxima_core.owner_rls_epoch IS
    'Non-optional marker proving that the owner-RLS migration and complete policy census ran.';

-- The erased-target witness is ownerless by design.  Its four existing
-- lifecycle triggers remain the only writers; this narrowly-scoped definer is
-- the one bridge that lets those triggers insert under FORCE RLS.  It is not a
-- general SQL executor: fixed search_path, no caller-controlled SQL, and the
-- trigger's identity marker still gates the insert.
CREATE OR REPLACE FUNCTION proxima_core.record_erased_pin_target(
    target uuid,
    target_kind proxima_core.pin_target_kind
)
RETURNS void
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core
AS $record_erased$
DECLARE
    existing_kind proxima_core.pin_target_kind;
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_pin_targets(ARRAY[target]);
    SELECT kind INTO existing_kind
      FROM proxima_core.erased_pin_target
     WHERE t = target;
    IF FOUND THEN
        IF existing_kind <> target_kind THEN
            RAISE EXCEPTION
                'erased pin target % already records kind %, not %',
                target, existing_kind, target_kind
                USING ERRCODE = '23505';
        END IF;
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN;
    END IF;
    PERFORM set_config('proxima_core.erased_pin_target_writer', target::text, true);
    INSERT INTO proxima_core.erased_pin_target (t, kind)
    VALUES (target, target_kind);
    PERFORM set_config('proxima_core.erased_pin_target_writer', '', true);
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
END;
$record_erased$;

REVOKE ALL ON FUNCTION proxima_core.record_erased_pin_target(uuid, proxima_core.pin_target_kind) FROM PUBLIC;
ALTER FUNCTION proxima_core.record_erased_pin_target(uuid, proxima_core.pin_target_kind) OWNER TO CURRENT_USER;

-- Integrity triggers must see cross-owner targets and historical witnesses.
-- These are the existing fixed trigger routines, never caller-facing SQL
-- helpers.  Bodies are 0005's (0001's for pins_have_grounding_support) plus
-- the record_erased_pin_target scope bridge: save the caller's scope, bind
-- platform transaction-locally, restore before every normal RETURN.  An error
-- unwinds the binding with the aborting (sub)transaction.  No function-level
-- SET app.proxima_scope: PostgreSQL requires a superuser-issued
-- GRANT SET ON PARAMETER for that, which a nonsuperuser owner cannot declare.
CREATE OR REPLACE FUNCTION proxima_core.assert_erased_pin_target_insert()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core
AS $$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF pg_trigger_depth() < 2
       OR current_setting('proxima_core.erased_pin_target_writer', true)
              IS DISTINCT FROM NEW.t::text
    THEN
        RAISE EXCEPTION
            'erased_pin_target is written only by a target deletion trigger'
            USING ERRCODE = '42501';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM proxima_core.memory m
         WHERE m.t = NEW.t AND m.kind::text = NEW.kind::text
        UNION ALL
        SELECT 1 FROM proxima_core.cooled c
         WHERE c.t = NEW.t AND c.kind::text = NEW.kind::text
        UNION ALL
        SELECT 1 FROM proxima_core.goal g
         WHERE g.t = NEW.t AND NEW.kind = 'goal'
    ) THEN
        RAISE EXCEPTION
            'erased_pin_target % must match the live row being deleted', NEW.t
            USING ERRCODE = '23503';
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.memory_erase_witness()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core
AS $$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_pin_targets(ARRAY[OLD.t]);
    -- Memory -> cooled is forget, not hard erase. Hydration's cooled row is
    -- likewise present when it deletes the cooled half, so neither transition
    -- manufactures a historical witness.
    IF NOT EXISTS (SELECT 1 FROM proxima_core.cooled WHERE t = OLD.t) THEN
        PERFORM proxima_core.record_erased_pin_target(
            OLD.t, OLD.kind::text::proxima_core.pin_target_kind
        );
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN OLD;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.cooled_erase_witness()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core
AS $$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_pin_targets(ARRAY[OLD.t]);
    -- Cooled -> Memory is hydration, not hard erase.
    IF NOT EXISTS (SELECT 1 FROM proxima_core.memory WHERE t = OLD.t) THEN
        PERFORM proxima_core.record_erased_pin_target(
            OLD.t, OLD.kind::text::proxima_core.pin_target_kind
        );
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN OLD;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.goal_erase_witness()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core
AS $$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_pin_targets(ARRAY[OLD.t]);
    PERFORM proxima_core.record_erased_pin_target(OLD.t, 'goal');
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN OLD;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.cooled_identity_seal()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core
AS $$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_pin_targets(ARRAY[NEW.t]);
    IF EXISTS (SELECT 1 FROM proxima_core.erased_pin_target WHERE t = NEW.t)
       OR EXISTS (SELECT 1 FROM proxima_core.goal WHERE t = NEW.t)
    THEN
        RAISE EXCEPTION 'cooled target % collides with a goal or erased target', NEW.t
            USING ERRCODE = '23505';
    END IF;

    -- Let the column CHECK name malformed arrays. Legacy cooled rows may have
    -- NULL declaration arrays; new rows carry all three arrays.
    IF (NEW.origins IS NOT NULL AND array_position(NEW.origins, NULL) IS NOT NULL)
       OR (NEW.refs IS NOT NULL AND array_position(NEW.refs, NULL) IS NOT NULL)
       OR (NEW.goal_refs IS NOT NULL AND array_position(NEW.goal_refs, NULL) IS NOT NULL)
    THEN
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;

    IF NOT EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.t = NEW.t
           AND m.handle = NEW.handle
           AND m.owner_id = NEW.owner_id
           AND m.kind = NEW.kind
           AND m.blob_id IS NOT DISTINCT FROM NEW.blob_id
           AND m.content_id IS NOT DISTINCT FROM NEW.content_id
           AND m.source_id IS NOT DISTINCT FROM NEW.source_id
           AND m.ingest_key IS NOT DISTINCT FROM NEW.ingest_key
           AND m.origins IS NOT DISTINCT FROM NEW.origins
           AND m.refs IS NOT DISTINCT FROM NEW.refs
           AND m.goal_refs IS NOT DISTINCT FROM NEW.goal_refs
    ) THEN
        RAISE EXCEPTION 'cooled row % does not seal its hot Memory', NEW.t
            USING ERRCODE = '23514';
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$$;

-- The SQL backstop follows the same set-first order as the Rust forget path:
-- source and every hot non-Fact depender take the lifecycle advisory before
-- any depender row lock used by the grounding check.
CREATE OR REPLACE FUNCTION proxima_core.cooled_forget_grounding()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core
AS $$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
    dependent_ids uuid[];
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF NEW.kind = 'fact' THEN
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;
    SELECT COALESCE(array_agg(m.t ORDER BY m.t), '{}'::uuid[])
      INTO dependent_ids
      FROM proxima_core.memory m
     WHERE m.kind <> 'fact'
       AND m.t <> NEW.t
       AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t]);
    PERFORM proxima_core.lock_pin_targets(ARRAY[NEW.t] || dependent_ids);
    IF EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.kind <> 'fact'
           AND m.t <> NEW.t
           AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t])
           AND NOT (m.t = ANY (dependent_ids))
    ) THEN
        RAISE EXCEPTION
            'forget depender footprint grew after lifecycle lock acquisition'
            USING ERRCODE = '40001';
    END IF;
    -- Lock dependers only after the complete lifecycle set is held.
    PERFORM 1
      FROM proxima_core.memory m
     WHERE m.kind <> 'fact'
       AND m.t <> NEW.t
       AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t])
     ORDER BY m.t
     FOR UPDATE;
    IF EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.kind <> 'fact'
           AND m.t <> NEW.t
           AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t])
           AND NOT proxima_core.pins_have_grounding_support(
                 m.origins || m.refs, NEW.t, NEW.kind
               )
    ) THEN
        RAISE EXCEPTION 'forget would leave an ungrounded memory'
            USING ERRCODE = '23514';
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$$;

-- Goal and wake declarations are also target admissions. Rust validates their
-- live endpoint kinds; this database guard closes the witness hole when a
-- caller reaches the tables without the engine.
CREATE OR REPLACE FUNCTION proxima_core.goal_pin_target_checks()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core
AS $$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
    pin uuid;
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_pin_targets(
        array_remove(
            ARRAY[NEW.t, NEW.close_fact_t, NEW.assignment_t, NEW.write_act_t],
            NULL
        ) || NEW.dependency_t || NEW.evidence_t
    );
    SELECT e.t INTO pin
      FROM proxima_core.erased_pin_target e
     WHERE e.t = ANY (
         array_remove(
             ARRAY[NEW.close_fact_t, NEW.assignment_t, NEW.write_act_t], NULL
         ) || NEW.dependency_t || NEW.evidence_t
     )
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'goal declaration names erased target %', pin
            USING ERRCODE = '23503';
    END IF;
    IF EXISTS (SELECT 1 FROM proxima_core.erased_pin_target WHERE t = NEW.t)
       OR EXISTS (SELECT 1 FROM proxima_core.memory WHERE t = NEW.t)
       OR EXISTS (SELECT 1 FROM proxima_core.cooled WHERE t = NEW.t)
    THEN
        RAISE EXCEPTION 'goal t % collides with an existing entity', NEW.t
            USING ERRCODE = '23505';
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.wake_pin_target_checks()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core
AS $$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
    pin uuid;
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_pin_targets(
        array_remove(ARRAY[NEW.trigger_t], NULL) || NEW.hard_memory_t
    );
    SELECT e.t INTO pin
      FROM proxima_core.erased_pin_target e
     WHERE e.t = ANY (
         array_remove(ARRAY[NEW.trigger_t], NULL) || NEW.hard_memory_t
     )
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'wake configuration names erased target %', pin
            USING ERRCODE = '23503';
    END IF;
    IF NEW.trigger_t IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM proxima_core.memory WHERE t = NEW.trigger_t)
    THEN
        RAISE EXCEPTION 'wake trigger memory does not exist'
            USING ERRCODE = '23503';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM unnest(NEW.hard_memory_t) AS h(t)
         WHERE NOT EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.t = h.t)
    ) THEN
        RAISE EXCEPTION 'wake hard context memory does not exist'
            USING ERRCODE = '23503';
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$$;

-- Origins and references have different target vocabularies. Keep the
-- existence checks set-based and lock hot targets before checking them: a
-- concurrent erase must either wait for this admission or win before it, not
-- disappear a target between the check and insertion of the source row.
CREATE OR REPLACE FUNCTION proxima_core.memory_pin_checks()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core
AS $$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
    pin uuid;
    pin_handle uuid;
    historical_restore boolean;
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_pin_targets(
        ARRAY[NEW.t] || NEW.origins || NEW.refs || NEW.goal_refs
    );

    IF NEW.origins <> '{}' OR NEW.refs <> '{}' THEN
        PERFORM 1
          FROM proxima_core.memory
         WHERE t = ANY (NEW.origins || NEW.refs)
         ORDER BY t
         FOR SHARE;
    END IF;
    IF NEW.goal_refs <> '{}' THEN
        PERFORM 1
          FROM proxima_core.goal
         WHERE t = ANY (NEW.goal_refs)
         ORDER BY t
         FOR SHARE;
    END IF;

    IF EXISTS (SELECT 1 FROM proxima_core.goal WHERE t = NEW.t)
       OR EXISTS (SELECT 1 FROM proxima_core.erased_pin_target WHERE t = NEW.t)
    THEN
        RAISE EXCEPTION 'memory t % is already a Goal or erased target', NEW.t
            USING ERRCODE = '23505';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM proxima_core.cooled c
         WHERE c.t = NEW.t
           AND c.handle = NEW.handle
           AND c.owner_id = NEW.owner_id
           AND c.kind = NEW.kind
           AND c.source_id IS NOT DISTINCT FROM NEW.source_id
           AND c.ingest_key IS NOT DISTINCT FROM NEW.ingest_key
           AND c.blob_id IS NOT DISTINCT FROM NEW.blob_id
           AND c.content_id IS NOT DISTINCT FROM NEW.content_id
           AND c.origins IS NOT NULL
           AND c.refs IS NOT NULL
           AND c.goal_refs IS NOT NULL
           AND c.origins = NEW.origins
           AND c.refs = NEW.refs
           AND c.goal_refs = NEW.goal_refs
    ) INTO historical_restore;

    -- A sealed cooled row may only be reinserted with the exact identity it
    -- carried. This prevents a direct INSERT from laundering a new row
    -- through a cooled identity.
    IF EXISTS (
        SELECT 1
          FROM proxima_core.cooled c
         WHERE c.t = NEW.t
           AND NOT (
               c.handle = NEW.handle
               AND c.owner_id = NEW.owner_id
               AND c.kind = NEW.kind
               AND c.source_id IS NOT DISTINCT FROM NEW.source_id
               AND c.ingest_key IS NOT DISTINCT FROM NEW.ingest_key
               AND c.blob_id IS NOT DISTINCT FROM NEW.blob_id
               AND c.content_id IS NOT DISTINCT FROM NEW.content_id
           )
    ) THEN
        RAISE EXCEPTION 'memory insert % does not match its cooled identity seal', NEW.t
            USING ERRCODE = '23514';
    END IF;

    -- Nullable arrays are legacy rows. A row with no declaration arrays is
    -- history from before migration 0003; any partial declaration is malformed and must not
    -- fall onto the live-target path, where it could launder a changed pin.
    IF EXISTS (
        SELECT 1
          FROM proxima_core.cooled c
         WHERE c.t = NEW.t
           AND (
               c.origins IS NOT NULL
               OR c.refs IS NOT NULL
               OR c.goal_refs IS NOT NULL
           )
           AND NOT (
               c.origins IS NOT DISTINCT FROM NEW.origins
               AND c.refs IS NOT DISTINCT FROM NEW.refs
               AND c.goal_refs IS NOT DISTINCT FROM NEW.goal_refs
           )
    ) THEN
        RAISE EXCEPTION 'memory insert % does not match its cooled restoration seal', NEW.t
            USING ERRCODE = '23514';
    END IF;

    IF NEW.kind = 'fact'
       AND NEW.origins = '{}'
       AND NEW.refs = '{}'
       AND NEW.goal_refs = '{}'
    THEN
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;

    -- Goal references never provide F/A/P grounding. A historical restore is
    -- the sole exception: its exact cooled seal already proves the original
    -- admitted declaration and its erased witness preserves target kind.
    IF NEW.kind <> 'fact' AND NOT historical_restore
       AND NOT proxima_core.pins_have_grounding_support(
             NEW.origins || NEW.refs, NULL, NULL
           )
    THEN
        RAISE EXCEPTION 'non-fact must pin a hot memory or a cooled fact'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.origins = '{}' AND NEW.refs = '{}' AND NEW.goal_refs = '{}' THEN
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;

    -- Origins are always Memory targets. A historical restore may use only a
    -- matching non-Goal witness; a Goal witness must never satisfy layering.
    SELECT p.id INTO pin
      FROM unnest(NEW.origins) AS p(id)
      LEFT JOIN proxima_core.memory m ON m.t = p.id
      LEFT JOIN proxima_core.cooled c ON c.t = p.id
     LEFT JOIN proxima_core.erased_pin_target e ON e.t = p.id
     WHERE m.t IS NULL AND c.t IS NULL
       AND (
           e.t IS NULL
           OR NOT (
               historical_restore
               AND e.kind IN ('fact', 'abstraction', 'perspective')
           )
       )
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'origin pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    -- `refs` carries only Memory targets after the 0004 split.
    SELECT p.id INTO pin
      FROM unnest(NEW.refs) AS p(id)
      LEFT JOIN proxima_core.memory m ON m.t = p.id
      LEFT JOIN proxima_core.cooled c ON c.t = p.id
     LEFT JOIN proxima_core.erased_pin_target e ON e.t = p.id
     WHERE m.t IS NULL AND c.t IS NULL
       AND (
           e.t IS NULL
           OR NOT (
               historical_restore
               AND e.kind IN ('fact', 'abstraction', 'perspective')
           )
       )
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'reference pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    -- `goal_refs` carries only Goal targets, including a retained Goal
    -- witness for an exact historical restore.
    SELECT p.id INTO pin
      FROM unnest(NEW.goal_refs) AS p(id)
      LEFT JOIN proxima_core.goal g ON g.t = p.id
      LEFT JOIN proxima_core.erased_pin_target e
        ON e.t = p.id AND e.kind = 'goal'
     WHERE g.t IS NULL
       AND (e.t IS NULL OR NOT historical_restore)
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'goal reference pin % does not exist as a Goal', pin
            USING ERRCODE = '23503';
    END IF;

    IF NOT historical_restore THEN
        SELECT m.handle INTO pin_handle
          FROM proxima_core.memory m
          JOIN proxima_core.closed_handle c ON c.handle = m.handle
         WHERE m.t = ANY (NEW.origins || NEW.refs)
         LIMIT 1;
        IF FOUND THEN
            RAISE EXCEPTION 'closed_handle: no new pin to %', pin_handle
                USING ERRCODE = '23514';
        END IF;
    END IF;

    IF NEW.kind = 'abstraction' AND NEW.origins <> '{}' THEN
        IF EXISTS (
            SELECT 1
              FROM unnest(NEW.origins) AS o(id)
             WHERE NOT EXISTS (
                       SELECT 1 FROM proxima_core.memory m
                        WHERE m.t = o.id AND m.kind IN ('fact', 'abstraction')
                   )
               AND NOT EXISTS (
                       SELECT 1 FROM proxima_core.cooled c
                        WHERE c.t = o.id AND c.kind IN ('fact', 'abstraction')
                   )
               AND NOT (historical_restore AND EXISTS (
                       SELECT 1 FROM proxima_core.erased_pin_target e
                        WHERE e.t = o.id AND e.kind IN ('fact', 'abstraction')
                   ))
        ) THEN
            RAISE EXCEPTION 'abstraction origins must be fact or abstraction t'
                USING ERRCODE = '23514';
        END IF;
    ELSIF NEW.kind = 'perspective' AND NEW.origins <> '{}' THEN
        IF EXISTS (
            SELECT 1
              FROM unnest(NEW.origins) AS o(id)
             WHERE NOT EXISTS (
                       SELECT 1 FROM proxima_core.memory m
                        WHERE m.t = o.id AND m.kind = 'abstraction'
                   )
               AND NOT EXISTS (
                       SELECT 1 FROM proxima_core.cooled c
                        WHERE c.t = o.id AND c.kind = 'abstraction'
                   )
               AND NOT (historical_restore AND EXISTS (
                       SELECT 1 FROM proxima_core.erased_pin_target e
                        WHERE e.t = o.id AND e.kind = 'abstraction'
                   ))
        ) THEN
            RAISE EXCEPTION 'perspective origins must be abstraction t'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$$;

-- Was LANGUAGE sql; the scope bridge needs PL/pgSQL.  Same VOLATILE query.
CREATE OR REPLACE FUNCTION proxima_core.pins_have_grounding_support(
    pins uuid[],
    cooling uuid,
    cooling_kind proxima_core.memory_kind
)
RETURNS boolean
LANGUAGE plpgsql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core
AS $$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
    supported boolean;
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    SELECT EXISTS (
        SELECT 1
          FROM unnest(pins) AS p(id)
         WHERE CASE
                 WHEN cooling IS NOT NULL AND p.id = cooling THEN
                   cooling_kind = 'fact'
                 ELSE
                   EXISTS (SELECT 1 FROM proxima_core.memory h WHERE h.t = p.id)
                   OR EXISTS (
                        SELECT 1 FROM proxima_core.cooled c
                         WHERE c.t = p.id AND c.kind = 'fact'
                   )
               END
    ) INTO supported;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN supported;
END;
$$;

DO $rls_trigger_bridge$
DECLARE
    routine text;
    routines constant text[] := ARRAY[
        'proxima_core.assert_erased_pin_target_insert()',
        'proxima_core.memory_erase_witness()',
        'proxima_core.cooled_erase_witness()',
        'proxima_core.goal_erase_witness()',
        'proxima_core.cooled_identity_seal()',
        'proxima_core.goal_pin_target_checks()',
        'proxima_core.wake_pin_target_checks()',
        'proxima_core.memory_pin_checks()',
        'proxima_core.cooled_forget_grounding()',
        'proxima_core.pins_have_grounding_support(uuid[],uuid,proxima_core.memory_kind)'
    ];
BEGIN
    FOREACH routine IN ARRAY routines
    LOOP
        EXECUTE format('ALTER FUNCTION %s OWNER TO CURRENT_USER', routine);
        EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', routine);
    END LOOP;
END
$rls_trigger_bridge$;
