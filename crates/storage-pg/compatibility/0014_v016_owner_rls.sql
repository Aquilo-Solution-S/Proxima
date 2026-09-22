-- Owner RLS activation. Apply only after every live host runs the v0.0.15 bridge.
-- This exact successor is checksum-approved by the bridge, but is not run by it.

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
    IF NOT has_parameter_privilege(current_user, 'app.proxima_scope', 'SET') THEN
        RAISE EXCEPTION 'owner RLS migration role lacks SET on app.proxima_scope'
            USING HINT = 'GRANT SET ON PARAMETER app.proxima_scope TO the migration role';
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
-- helpers.  Function-local proconfig restores the request scope on return;
-- no platform setting escapes the statement/transaction that invoked it.
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
        EXECUTE format('ALTER FUNCTION %s SECURITY DEFINER', routine);
        EXECUTE format('ALTER FUNCTION %s SET search_path = pg_catalog, proxima_core', routine);
        EXECUTE format('ALTER FUNCTION %s SET app.proxima_scope = ''platform''', routine);
        EXECUTE format('REVOKE ALL ON FUNCTION %s FROM PUBLIC', routine);
    END LOOP;
END
$rls_trigger_bridge$;
