-- Historical definitions are immutable replay inputs, not runtime aliases.
SELECT $rename_history_functions$-- Functions and aggregate definitions owned by the proxima_core catalog.
-- This file is the source for current core function definitions.
-- Definitions are replaceable; runtime and platform grants come from defaults.

-- Assertion functions

CREATE OR REPLACE FUNCTION proxima_core.assert_declared_sidecar_present() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_ARGV[0];
    present boolean;
BEGIN
    IF NOT (surface = ANY (NEW.sidecar_tables)) THEN
        RETURN NULL;
    END IF;
    EXECUTE format('SELECT EXISTS (SELECT 1 FROM %s WHERE %I = $1)', surface, TG_ARGV[1])
       INTO present
      USING NEW.t;
    IF NOT present THEN
        RAISE EXCEPTION
            'memory % declares % in memory.sidecar_tables with no row in that table',
            NEW.t, surface
            USING ERRCODE = '23503',
                  HINT = 'a stamp with no row leaves the Memory declaring sidecar data '
                         || 'it does not have, so its reads, export and transfer come up '
                         || 'short; write through Engine::unit_of_work, which stamps '
                         || 'exactly the tables the write inserts into, in the same '
                         || 'transaction';
    END IF;
    RETURN NULL;
END;
$$;

-- FORGET-POLICY: includes-forgotten (pin integrity holds for forgotten targets) applies to every read in this function
CREATE OR REPLACE FUNCTION proxima_core.assert_erased_pin_target_insert()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
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
$function$;
REVOKE ALL ON FUNCTION proxima_core.assert_erased_pin_target_insert() FROM PUBLIC;

-- FORGET-POLICY: includes-forgotten (a forgotten memory keeps its sidecar rows) applies to every read in this function
CREATE OR REPLACE FUNCTION proxima_core.assert_memory_declares_sidecar() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME;
    memory_t uuid := (to_jsonb(NEW) ->> TG_ARGV[0])::uuid;
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.t = memory_t
           AND m.sidecar_tables @> ARRAY[surface]
    ) THEN
        RAISE EXCEPTION
            'sidecar row in % for memory %, which does not declare % in memory.sidecar_tables',
            surface, memory_t, surface
            USING ERRCODE = '23503',
                  HINT = 'forget, owner erase and owner export all reach a sidecar row '
                         || 'through memory.sidecar_tables and nowhere else, so an undeclared '
                         || 'row is reachable by none of them; write through '
                         || 'Engine::unit_of_work, which stamps the memory row with every '
                         || 'table the write touches, in the same transaction and before the '
                         || 'sidecar row';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.assert_owner_rls_census(target_schema text)
 RETURNS void
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    base_table text;
    counted integer := 0;
BEGIN
    IF target_schema IS NULL OR to_regnamespace(quote_ident(target_schema)) IS NULL THEN
        RAISE EXCEPTION 'assert_owner_rls_census: schema % does not exist', COALESCE(target_schema, 'NULL');
    END IF;
    FOR base_table IN
        SELECT c.relname
          FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = target_schema AND c.relkind IN ('r', 'p')
         ORDER BY c.relname
    LOOP
        PERFORM proxima_core.owner_rls_census_table(target_schema, base_table);
        counted := counted + 1;
    END LOOP;
    IF counted = 0 THEN
        RAISE EXCEPTION 'assert_owner_rls_census: schema % has no tables', target_schema;
    END IF;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.assert_owner_rls_census(target_schema text) FROM PUBLIC;
COMMENT ON FUNCTION proxima_core.assert_owner_rls_census(target_schema text) IS 'Raises on the first base table of the schema without ENABLE/FORCE RLS, exactly three policies and the owner-role platform policy.';

-- FORGET-POLICY: includes-forgotten (a forgotten memory keeps its sidecar rows) applies to every read in this function
CREATE OR REPLACE FUNCTION proxima_core.assert_row_not_still_declared() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_ARGV[0];
    memory_t uuid := (to_jsonb(OLD) ->> TG_ARGV[1])::uuid;
BEGIN
    IF EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.t = memory_t
           AND m.sidecar_tables @> ARRAY[surface]
    ) THEN
        RAISE EXCEPTION
            'memory % still declares % in memory.sidecar_tables, so the row it declares there may not be deleted',
            memory_t, surface
            USING ERRCODE = '23503',
                  HINT = 'a stamp whose row was deleted leaves the Memory declaring '
                         || 'sidecar data that is gone, so its reads, export and transfer '
                         || 'come up short and nothing can repair it; delete the memory row '
                         || 'in the same transaction, which is what owner erase does';
    END IF;
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.assert_sidecar_stamp_declared()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
DECLARE
    undeclared text;
BEGIN
    SELECT stamped INTO undeclared
      FROM unnest(NEW.sidecar_tables) AS stamped
     WHERE NOT EXISTS (
               SELECT 1
                 FROM proxima_core.flavor_surface fs
                WHERE fs.table_name = stamped
           )
     LIMIT 1;
    IF undeclared IS NOT NULL THEN
        RAISE EXCEPTION
            'memory.sidecar_tables names %, which no flavor declares in proxima_core.flavor_surface',
            undeclared
            USING ERRCODE = '23503';
    END IF;
    RETURN NEW;
END;
$function$;

-- Forget functions

-- Fires on memory.forgotten_at being set (#488). The name predates the
-- forget-is-a-state change; the trigger that calls it is memory_forget_grounding.
-- Since S13 it only takes the footprint and depender locks; it refuses nothing.
-- FORGET-POLICY: includes-forgotten (forget grounding: locks every depender, checks the live ones) applies to every read in this function
CREATE OR REPLACE FUNCTION proxima_core.cooled_forget_grounding()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
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
    -- No grounding refusal since #488 S13: a forgotten memory still grounds
    -- what was derived from it. The locks above keep forget serialized with
    -- a concurrent derive over the same targets.
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$function$;
REVOKE ALL ON FUNCTION proxima_core.cooled_forget_grounding() FROM PUBLIC;

-- Immutability functions

CREATE OR REPLACE FUNCTION proxima_core.enforce_installation_immutable()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    RAISE EXCEPTION 'write-once: the installation identity cannot be changed or removed'
        USING ERRCODE = '25006';
END;
$function$;

CREATE OR REPLACE FUNCTION proxima_core.enforce_publication_immutable()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    IF NEW.t IS DISTINCT FROM OLD.t
        OR NEW.event_id IS DISTINCT FROM OLD.event_id
        OR NEW.event_type IS DISTINCT FROM OLD.event_type
        OR NEW.envelope IS DISTINCT FROM OLD.envelope
        OR NEW.envelope_digest IS DISTINCT FROM OLD.envelope_digest
        OR NEW.owner_id IS DISTINCT FROM OLD.owner_id
        OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
        OR NEW.schema_version IS DISTINCT FROM OLD.schema_version
        OR NEW.captured_at IS DISTINCT FROM OLD.captured_at
    THEN
        RAISE EXCEPTION
            'append-only: publication_outbox does not accept UPDATE of the captured event'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION proxima_core.enforce_publication_origin_immutable()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    RAISE EXCEPTION 'append-only: publication_origin identity cannot be updated'
        USING ERRCODE = '25006';
END;
$function$;

CREATE OR REPLACE FUNCTION proxima_core.enforce_row_append_only()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    RAISE EXCEPTION 'append-only: % does not accept UPDATE', TG_TABLE_NAME
        USING ERRCODE = '25006';
END;
$function$;

-- Embedding functions

-- #519: a head deleted or moved to another owner leaves index debt in the
-- same transaction (migration 0050 describes the queue). One row per key:
-- the newest change replaces an older one. A statement trigger writes a whole
-- erase's deletes in one insert; an owner move, which only a transfer makes,
-- is one row's.
CREATE OR REPLACE FUNCTION proxima_core.embedding_heads_deleted_index_debt() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    INSERT INTO proxima_core.vector_index_debt (entity_id, model_id, dim, owner_id, op)
    SELECT entity_id, model_id, dim, owner_id, 'delete' FROM deleted_heads
    ON CONFLICT (entity_id, model_id, dim) DO UPDATE
        SET owner_id = EXCLUDED.owner_id, op = EXCLUDED.op, enqueued_at = now(),
            xact = pg_current_xact_id();
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.embedding_head_moved_index_debt() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    INSERT INTO proxima_core.vector_index_debt (entity_id, model_id, dim, owner_id, op)
    VALUES (NEW.entity_id, NEW.model_id, NEW.dim, NEW.owner_id, 'owner')
    ON CONFLICT (entity_id, model_id, dim) DO UPDATE
        SET owner_id = EXCLUDED.owner_id, op = EXCLUDED.op, enqueued_at = now(),
            xact = pg_current_xact_id();
    RETURN NULL;
END;
$$;

-- Erasure functions

CREATE OR REPLACE FUNCTION proxima_core.erased_pin_target_append_only()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    RAISE EXCEPTION 'erased_pin_target is append-only'
        USING ERRCODE = '25006';
END;
$function$;

CREATE OR REPLACE FUNCTION proxima_core.record_erased_pin_target(target uuid, target_kind proxima_core.pin_target_kind)
 RETURNS void
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
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
$function$;
REVOKE ALL ON FUNCTION proxima_core.record_erased_pin_target(target uuid, target_kind proxima_core.pin_target_kind) FROM PUBLIC;

-- Goal functions

CREATE OR REPLACE FUNCTION proxima_core.goal_erase_witness()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_pin_targets(ARRAY[OLD.t]);
    PERFORM proxima_core.record_erased_pin_target(OLD.t, 'goal');
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN OLD;
END;
$function$;
REVOKE ALL ON FUNCTION proxima_core.goal_erase_witness() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.goal_head_t_only()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
       OR NEW.owner_id IS DISTINCT FROM OLD.owner_id THEN
        RAISE EXCEPTION 'goal_head is frozen except t'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION proxima_core.goal_no_later_after_terminal()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
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
$function$;

CREATE OR REPLACE FUNCTION proxima_core.goal_parent_fixed()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    -- #514: a series keeps the parent it was decomposed under.
    IF EXISTS (
        SELECT 1 FROM proxima_core.goal g
         WHERE g.handle = NEW.handle
           AND g.parent_handle IS DISTINCT FROM NEW.parent_handle
    ) THEN
        RAISE EXCEPTION 'goal series parent is fixed' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$function$;

-- FORGET-POLICY: includes-forgotten (pin integrity holds for forgotten targets) applies to every read in this function
CREATE OR REPLACE FUNCTION proxima_core.goal_pin_target_checks()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
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
    THEN
        RAISE EXCEPTION 'goal t % collides with an existing entity', NEW.t
            USING ERRCODE = '23505';
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$function$;
REVOKE ALL ON FUNCTION proxima_core.goal_pin_target_checks() FROM PUBLIC;

-- Group functions

-- The platform gateway acquires all trees before these ordered kind and
-- projection keys. Keep the key construction beside the membership writers.
CREATE OR REPLACE FUNCTION proxima_core.lock_authority_targets(p_owners uuid[], p_subjects uuid[])
 RETURNS void
 LANGUAGE plpgsql VOLATILE SECURITY INVOKER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    target uuid;
BEGIN
    FOR target IN SELECT DISTINCT id FROM unnest(p_owners) AS ids(id) ORDER BY id
    LOOP
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-owner-kind:' || target::text, 0));
    END LOOP;
    FOR target IN SELECT DISTINCT id FROM unnest(p_subjects) AS ids(id) ORDER BY id
    LOOP
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-group-projection:' || target::text, 0));
    END LOOP;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.lock_authority_targets(uuid[], uuid[]) FROM PUBLIC;

-- The authenticated subject's authority projection is distinct from memory
-- owner reach. VOLATILE gives the query after a lock wait a fresh snapshot.
CREATE OR REPLACE FUNCTION proxima_core.read_subject_authority(p_subject uuid)
 RETURNS TABLE(group_id uuid, read_ceiling proxima_core.access_ceiling,
               write_ceiling proxima_core.access_ceiling, manage boolean)
 LANGUAGE plpgsql VOLATILE SECURITY INVOKER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    previous_subject text := current_setting('app.authority_subject', true);
BEGIN
    IF current_setting('app.proxima_scope', true) IS DISTINCT FROM 'owner'
       OR p_subject IS NULL
       OR NULLIF(current_setting('app.subject', true), '')::uuid IS DISTINCT FROM p_subject
    THEN
        RAISE EXCEPTION USING ERRCODE = '42501',
            MESSAGE = 'authority reads require the authenticated subject';
    END IF;
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING ERRCODE = '25001',
            MESSAGE = 'authority reads require READ COMMITTED';
    END IF;
    -- Membership writers take owner-kind before rebuilding the projection.
    -- The latter fence covers an empty projection and inherited memberships.
    PERFORM pg_advisory_xact_lock_shared(
        hashtextextended('proxima-owner-kind:' || p_subject::text, 0));
    PERFORM pg_advisory_xact_lock_shared(
        hashtextextended('proxima-group-projection:' || p_subject::text, 0));
    PERFORM set_config('app.authority_subject', p_subject::text, true);
    IF EXISTS (SELECT 1 FROM proxima_core.owners o
                WHERE o.owner_id = p_subject AND o.kind = 'group') THEN
        RAISE EXCEPTION USING ERRCODE = '42501',
            MESSAGE = 'authority subject must be a person';
    END IF;
    RETURN QUERY
        SELECT m.group_id, m.read_ceiling, m.write_ceiling, m.manage
          FROM proxima_core.group_role_projection m
         WHERE m.member_user_id = p_subject
         ORDER BY m.group_id;
    PERFORM set_config('app.authority_subject', COALESCE(previous_subject, ''), true);
EXCEPTION WHEN OTHERS THEN
    PERFORM set_config('app.authority_subject', COALESCE(previous_subject, ''), true);
    RAISE;
END
$function$;

CREATE OR REPLACE FUNCTION proxima_core.group_edge_before_write()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF TG_OP = 'DELETE' THEN
        PERFORM proxima_core.lock_group_tree(OLD.child_group_id);
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-owner-kind:' || OLD.child_group_id::text, 0));
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN OLD;
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.child_group_id, NEW.parent_group_id) <> (OLD.child_group_id, OLD.parent_group_id) THEN
        RAISE EXCEPTION 'group edge identity is immutable';
    END IF;
    PERFORM proxima_core.lock_group_trees(NEW.child_group_id, NEW.parent_group_id);
    IF NEW.child_group_id <= NEW.parent_group_id THEN
        PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.child_group_id::text, 0));
        PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.parent_group_id::text, 0));
    ELSE
        PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.parent_group_id::text, 0));
        PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.child_group_id::text, 0));
    END IF;
    IF NEW.child_group_id = NEW.parent_group_id THEN
        RAISE EXCEPTION 'a group cannot contain itself';
    END IF;
    IF EXISTS (
        WITH RECURSIVE descendants(group_id) AS (
            SELECT NEW.parent_group_id
          UNION ALL SELECT e.parent_group_id FROM descendants d
            JOIN proxima_core.group_edges e ON e.child_group_id = d.group_id
        ) SELECT 1 FROM descendants WHERE group_id = NEW.child_group_id
    ) THEN
        RAISE EXCEPTION 'group edge would create a cycle';
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.group_edge_before_write() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.group_lock_request_cleanup()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    DELETE FROM proxima_core.group_lock_request WHERE request_id = NEW.request_id;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NULL;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.group_lock_request_cleanup() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.group_lock_request_trigger()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF TG_OP = 'INSERT' THEN
        IF NEW.right_group_id IS NULL THEN
            PERFORM proxima_core.lock_group_tree(NEW.left_group_id);
        ELSE
            PERFORM proxima_core.lock_group_trees(NEW.left_group_id, NEW.right_group_id);
        END IF;
        IF NEW.right_group_id IS NULL THEN
            NEW.member_is_group := false;
        ELSE
            IF NEW.left_group_id <= NEW.right_group_id THEN
                PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.left_group_id::text, 0));
                PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.right_group_id::text, 0));
            ELSE
                PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.right_group_id::text, 0));
                PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.left_group_id::text, 0));
            END IF;
            SELECT EXISTS (
                SELECT 1 FROM proxima_core.owners
                 WHERE owner_id = NEW.right_group_id AND kind = 'group'
            ) INTO NEW.member_is_group;
        END IF;
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;
    RETURN OLD;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.group_lock_request_trigger() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.group_membership_before_write()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    member_kind proxima_core.owner_kind;
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_group_tree(COALESCE(NEW.group_id, OLD.group_id));
    IF TG_OP = 'DELETE' THEN
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-owner-kind:' || OLD.member_user_id::text, 0));
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN OLD;
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.group_id, NEW.member_user_id) <> (OLD.group_id, OLD.member_user_id) THEN
        RAISE EXCEPTION 'membership identity is immutable';
    END IF;
    PERFORM pg_advisory_xact_lock(
        hashtextextended('proxima-owner-kind:' || NEW.member_user_id::text, 0));
    SELECT kind INTO member_kind FROM proxima_core.owners WHERE owner_id = NEW.member_user_id;
    IF member_kind = 'group' THEN
        RAISE EXCEPTION 'owner % is a group; use the group edge write', NEW.member_user_id;
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.group_membership_before_write() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.group_projection_after_write()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    users uuid[];
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF TG_TABLE_NAME = 'group_memberships' THEN
        users := ARRAY[COALESCE(NEW.member_user_id, OLD.member_user_id)];
    ELSE
        WITH RECURSIVE descendants(group_id) AS (
            SELECT COALESCE(NEW.child_group_id, OLD.child_group_id)
          UNION ALL SELECT e.child_group_id FROM descendants d
            JOIN proxima_core.group_edges e ON e.parent_group_id = d.group_id
        )
        SELECT COALESCE(array_agg(DISTINCT m.member_user_id), '{}') INTO users
          FROM descendants d JOIN proxima_core.group_memberships m ON m.group_id = d.group_id;
    END IF;
    IF cardinality(users) > 0 THEN PERFORM proxima_core.rebuild_group_role_projection(users); END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NULL;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.group_projection_after_write() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.group_projection_write_guard()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
BEGIN
    IF current_setting('proxima.projection_maintenance', true) IS DISTINCT FROM 'on'
       OR pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION 'group_role_projection is maintained by membership triggers';
    END IF;
    RETURN COALESCE(NEW, OLD);
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.group_projection_write_guard() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.group_tree_root(p_group uuid)
 RETURNS uuid
 LANGUAGE sql
 STABLE SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
    WITH RECURSIVE parents(group_id, depth) AS (
        SELECT p_group, 0
      UNION ALL
        SELECT e.parent_group_id, p.depth + 1
          FROM parents p JOIN proxima_core.group_edges e
            ON e.child_group_id = p.group_id
    )
    SELECT group_id FROM parents ORDER BY depth DESC LIMIT 1
$function$;
REVOKE ALL ON FUNCTION proxima_core.group_tree_root(p_group uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.group_truncate_guard()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
BEGIN
    RAISE EXCEPTION '% cannot be truncated; membership projection requires row triggers', TG_TABLE_NAME;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.group_truncate_guard() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.rebuild_group_role_projection(p_users uuid[])
 RETURNS void
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    user_row record;
    user_id uuid;
    previous_scope text := current_setting('app.proxima_scope', true);
    previous_maintenance text := current_setting('proxima.projection_maintenance', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM set_config('proxima.projection_maintenance', 'on', true);
    FOR user_row IN
        SELECT DISTINCT u AS user_id
          FROM unnest(COALESCE(p_users, '{}')) AS u
         ORDER BY u
    LOOP
        user_id := user_row.user_id;
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-group-projection:' || user_id::text, 0));
        DELETE FROM proxima_core.group_role_projection WHERE member_user_id = user_id;
        INSERT INTO proxima_core.group_role_projection
        (member_user_id, group_id, read_ceiling, write_ceiling, manage)
    WITH RECURSIVE relation_roles(relation, read_rank, write_rank, manages) AS (
        VALUES ('admin'::proxima_core.membership_relation, 4, 4, true),
               ('editor'::proxima_core.membership_relation, 4, 3, false),
               ('viewer'::proxima_core.membership_relation, 4, 0, false),
               ('ingest'::proxima_core.membership_relation, 1, 1, false)
    ), paths(member_user_id, group_id, read_rank, write_rank, manages) AS (
        SELECT m.member_user_id, m.group_id,
               r.read_rank, r.write_rank, r.manages
          FROM proxima_core.group_memberships m
          JOIN relation_roles r ON r.relation = m.relation
         WHERE m.member_user_id = user_id
        UNION ALL
        SELECT p.member_user_id, e.parent_group_id,
               LEAST(p.read_rank, r.read_rank),
               LEAST(p.write_rank, r.write_rank),
               p.manages AND r.manages
          FROM paths p JOIN proxima_core.group_edges e ON e.child_group_id = p.group_id
          JOIN relation_roles r ON r.relation = e.relation
    ), folded AS (
        SELECT member_user_id, group_id, max(read_rank) AS read_rank,
               max(write_rank) AS write_rank, bool_or(manages) AS manages
          FROM paths GROUP BY member_user_id, group_id
    )
    SELECT member_user_id, group_id,
           (ARRAY['none','fact','abstraction','perspective','goal'])[read_rank + 1]::proxima_core.access_ceiling,
           (ARRAY['none','fact','abstraction','perspective','goal'])[write_rank + 1]::proxima_core.access_ceiling,
           manages
      FROM folded;
    END LOOP;
    PERFORM set_config('proxima.projection_maintenance', COALESCE(previous_maintenance, ''), true);
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.rebuild_group_role_projection(p_users uuid[]) FROM PUBLIC;

-- Owner-RLS installation functions

CREATE OR REPLACE FUNCTION proxima_core.install_owner_rls(target_schema text, owner_id_tables text[], fk_parent_tables text[], ownerless_tables text[], memory_owner_tables text[] DEFAULT '{}'::text[])
 RETURNS void
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    classified text[] := COALESCE(owner_id_tables, '{}') || COALESCE(fk_parent_tables, '{}')
        || COALESCE(ownerless_tables, '{}') || COALESCE(memory_owner_tables, '{}');
    listed text;
    relation record;
    parents jsonb := '{}';
    chain text;
    hops integer;
    class proxima_core.owner_rls_class;
    classes integer;
    installed integer := 0;
    follows text[];
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    IF target_schema IS NULL OR target_schema IN ('proxima_core', 'public', 'information_schema')
       OR target_schema LIKE 'pg\_%' THEN
        RAISE EXCEPTION 'install_owner_rls: % is not a flavor schema', COALESCE(target_schema, 'NULL');
    END IF;
    IF to_regnamespace(quote_ident(target_schema)) IS NULL THEN
        RAISE EXCEPTION 'install_owner_rls: schema % does not exist', target_schema;
    END IF;
    -- The host's Follows tables of this schema (sync_owner_follows): each
    -- gets the follows rule whatever class it is listed in, and needs no
    -- listing at all.
    PERFORM set_config('app.proxima_scope', 'platform', true);
    SELECT COALESCE(array_agg(substr(f.table_name, length(target_schema) + 2)), '{}')
      INTO follows
      FROM proxima_core.owner_follows AS f
     WHERE split_part(f.table_name, '.', 1) = target_schema;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    FOREACH listed IN ARRAY classified LOOP
        classes := (listed = ANY(COALESCE(owner_id_tables, '{}')))::integer
            + (listed = ANY(COALESCE(fk_parent_tables, '{}')))::integer
            + (listed = ANY(COALESCE(ownerless_tables, '{}')))::integer
            + (listed = ANY(COALESCE(memory_owner_tables, '{}')))::integer;
        IF classes > 1 THEN
            RAISE EXCEPTION 'install_owner_rls: %.% is classified % times', target_schema, listed, classes;
        END IF;
        IF NOT EXISTS (
            SELECT 1 FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
             WHERE n.nspname = target_schema AND c.relname = listed AND c.relkind IN ('r', 'p')
        ) THEN
            RAISE EXCEPTION 'install_owner_rls: classified table %.% does not exist', target_schema, listed;
        END IF;
    END LOOP;

    -- Each FK-parent table's owner-bearing FK, resolved before any policy.
    FOR relation IN
        SELECT c.oid, c.relname,
               EXISTS (
                   SELECT 1 FROM pg_attribute AS a
                    WHERE a.attrelid = c.oid AND a.attname = 'owner_id'
                      AND NOT a.attisdropped AND a.attnum > 0
               ) AS has_owner_id
          FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = target_schema AND c.relkind IN ('r', 'p')
           AND c.relname = ANY(COALESCE(fk_parent_tables, '{}'))
         ORDER BY c.relname
    LOOP
        IF relation.has_owner_id THEN
            RAISE EXCEPTION 'install_owner_rls: %.% has an owner_id column but is not in owner_id_tables',
                target_schema, relation.relname;
        END IF;
        parents := parents || jsonb_build_object(relation.relname,
            proxima_core.owner_rls_parent_fk('install_owner_rls', target_schema, relation.relname));
    END LOOP;
    FOR chain IN SELECT jsonb_object_keys(parents) LOOP
        listed := chain;
        hops := 0;
        WHILE parents -> listed ->> 'parent_schema' = target_schema
              AND parents ? (parents -> listed ->> 'parent_table') LOOP
            listed := parents -> listed ->> 'parent_table';
            hops := hops + 1;
            IF listed = chain OR hops > 1000 THEN
                RAISE EXCEPTION 'install_owner_rls: %.% reaches itself through its parent FKs; its policies would recurse',
                    target_schema, chain;
            END IF;
        END LOOP;
    END LOOP;

    FOR relation IN
        SELECT c.oid, n.nspname AS schema_name, c.relname,
               EXISTS (
                   SELECT 1 FROM pg_attribute AS a
                    WHERE a.attrelid = c.oid AND a.attname = 'owner_id'
                      AND NOT a.attisdropped AND a.attnum > 0
               ) AS has_owner_id
          FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = target_schema AND c.relkind IN ('r', 'p')
         ORDER BY c.relname
    LOOP
        IF relation.relname <> ALL(classified) AND relation.relname <> ALL(follows) THEN
            RAISE EXCEPTION 'owner RLS classification missing for %.%', relation.schema_name, relation.relname;
        END IF;
        IF relation.has_owner_id AND relation.relname <> ALL(COALESCE(owner_id_tables, '{}')
                || COALESCE(memory_owner_tables, '{}'))
           AND NOT (relation.relname = ANY(follows) AND relation.relname <> ALL(classified)) THEN
            RAISE EXCEPTION 'install_owner_rls: %.% has an owner_id column but is not in owner_id_tables',
                relation.schema_name, relation.relname;
        END IF;

        class := CASE
            WHEN relation.relname = ANY(COALESCE(owner_id_tables, '{}')) THEN 'owner_id'
            WHEN relation.relname = ANY(COALESCE(memory_owner_tables, '{}')) THEN 'memory_owner'
            WHEN relation.relname = ANY(COALESCE(ownerless_tables, '{}')) THEN 'ownerless'
            WHEN relation.relname = ANY(COALESCE(fk_parent_tables, '{}')) THEN 'fk_parent'
        END;
        IF class = 'owner_id' AND NOT relation.has_owner_id THEN
            RAISE EXCEPTION 'install_owner_rls: %.% is in owner_id_tables but has no owner_id column',
                relation.schema_name, relation.relname;
        END IF;

        IF relation.relname = ANY(follows) THEN
            IF class IS NOT NULL THEN
                RAISE NOTICE 'install_owner_rls: %.% is a Follows table; the host installs Follows owner RLS, so its class % is superseded',
                    relation.schema_name, relation.relname, class;
            END IF;
            PERFORM proxima_core.owner_rls_apply_follows(
                'install_owner_rls', relation.schema_name, relation.relname);
            installed := installed + 1;
            CONTINUE;
        END IF;
        PERFORM proxima_core.owner_rls_apply_class(
            'install_owner_rls', 'in ' || class::text || '_tables',
            relation.schema_name, relation.relname, class, 'owner_id');
        installed := installed + 1;
    END LOOP;

    IF installed = 0 THEN
        RAISE EXCEPTION 'install_owner_rls: schema % has no tables', target_schema;
    END IF;

    PERFORM proxima_core.assert_owner_rls_census(target_schema);
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.install_owner_rls(target_schema text, owner_id_tables text[], fk_parent_tables text[], ownerless_tables text[], memory_owner_tables text[]) FROM PUBLIC;
COMMENT ON FUNCTION proxima_core.install_owner_rls(target_schema text, owner_id_tables text[], fk_parent_tables text[], ownerless_tables text[], memory_owner_tables text[]) IS 'Owner-RLS policies for one flavor schema from its table classification; refuses an unclassified table. A table in proxima_core.owner_follows gets the host follows rule whatever class it is listed in, and may be left out (docs/build-a-flavor.md §2, docs/host-integration.md §Boot a composed host).';

CREATE OR REPLACE FUNCTION proxima_core.install_owner_rls_table(target_schema text, target_table text, class proxima_core.owner_rls_class, owner_column text DEFAULT 'owner_id'::text)
 RETURNS void
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    rel_oid oid;
    has_owner_id boolean;
    column_type oid;
    hop jsonb;
    next_table text;
    visited text[];
    follows boolean;
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    IF target_schema IS NULL OR target_schema IN ('proxima_core', 'public', 'information_schema')
       OR target_schema LIKE 'pg\_%' THEN
        RAISE EXCEPTION 'install_owner_rls_table: % is not a flavor schema', COALESCE(target_schema, 'NULL');
    END IF;
    IF to_regnamespace(quote_ident(target_schema)) IS NULL THEN
        RAISE EXCEPTION 'install_owner_rls_table: schema % does not exist', target_schema;
    END IF;
    SELECT c.oid INTO rel_oid
      FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
     WHERE n.nspname = target_schema AND c.relname = target_table AND c.relkind IN ('r', 'p');
    IF rel_oid IS NULL THEN
        RAISE EXCEPTION 'install_owner_rls_table: %.% is not an existing base table',
            target_schema, COALESCE(target_table, 'NULL');
    END IF;
    IF class IS NULL THEN
        RAISE EXCEPTION 'install_owner_rls_table: %.% has no class', target_schema, target_table;
    END IF;
    IF owner_column IS NULL THEN
        RAISE EXCEPTION 'install_owner_rls_table: %.% has a NULL owner_column', target_schema, target_table;
    END IF;
    IF class <> 'owner_id' AND owner_column <> 'owner_id' THEN
        RAISE EXCEPTION 'install_owner_rls_table: %.% is class %, whose policies read no owner column; owner_column belongs to class owner_id',
            target_schema, target_table, class;
    END IF;
    has_owner_id := EXISTS (
        SELECT 1 FROM pg_attribute AS a
         WHERE a.attrelid = rel_oid AND a.attname = 'owner_id'
           AND NOT a.attisdropped AND a.attnum > 0
    );
    IF has_owner_id AND class IN ('fk_parent', 'ownerless') THEN
        RAISE EXCEPTION 'install_owner_rls_table: %.% has an owner_id column but is class %; a table with owner_id is class owner_id or memory_owner',
            target_schema, target_table, class;
    END IF;
    PERFORM set_config('app.proxima_scope', 'platform', true);
    SELECT EXISTS (
        SELECT 1 FROM proxima_core.owner_follows AS f
         WHERE f.table_name = target_schema || '.' || target_table
    ) INTO follows;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    IF follows THEN
        RAISE NOTICE 'install_owner_rls_table: %.% is a Follows table; the host installs Follows owner RLS, so its class % is superseded',
            target_schema, target_table, class;
        PERFORM proxima_core.owner_rls_apply_follows('install_owner_rls_table', target_schema, target_table);
        PERFORM proxima_core.owner_rls_census_table(target_schema, target_table);
        RETURN;
    END IF;

    CASE class
        WHEN 'owner_id' THEN
            SELECT a.atttypid INTO column_type
              FROM pg_attribute AS a
             WHERE a.attrelid = rel_oid AND a.attname = owner_column
               AND NOT a.attisdropped AND a.attnum > 0;
            IF NOT FOUND THEN
                RAISE EXCEPTION 'install_owner_rls_table: %.% is class owner_id but has no % column',
                    target_schema, target_table, owner_column;
            END IF;
            IF column_type <> 'uuid'::regtype THEN
                RAISE EXCEPTION 'install_owner_rls_table: %.%.% is %, not uuid; an owner column holds a proxima_core.owners id, so store OwnerRef::stable_key_uuid() in a uuid column and keep the personal:<uuid> / group:<uuid> string at the edge',
                    target_schema, target_table, owner_column, format_type(column_type, NULL);
            END IF;
        WHEN 'fk_parent' THEN
            -- The chain of parents must not return to this table: the
            -- policies would recurse on every read. Only the parents are
            -- known, so follow them through every in-schema table without an
            -- owner_id column; proxima_core, a table with an owner column and
            -- a parent without a resolvable FK end the chain.
            hop := proxima_core.owner_rls_parent_fk('install_owner_rls_table', target_schema, target_table);
            visited := ARRAY[target_table];
            WHILE hop ->> 'parent_schema' = target_schema LOOP
                next_table := hop ->> 'parent_table';
                IF next_table = ANY(visited) THEN
                    RAISE EXCEPTION 'install_owner_rls_table: %.% reaches itself through its parent FKs; its policies would recurse',
                        target_schema, target_table;
                END IF;
                visited := visited || next_table;
                EXIT WHEN EXISTS (
                    SELECT 1 FROM pg_attribute AS a
                     WHERE a.attrelid = format('%I.%I', target_schema, next_table)::regclass
                       AND a.attname = 'owner_id' AND NOT a.attisdropped AND a.attnum > 0
                );
                hop := proxima_core.owner_rls_find_parent_fk(target_schema, next_table);
                EXIT WHEN hop IS NULL OR (NOT (hop ->> 'on_key')::boolean AND (hop ->> 'candidates')::integer > 1);
            END LOOP;
        WHEN 'ownerless' THEN
            NULL;
        WHEN 'memory_owner' THEN
            NULL;
    END CASE;

    PERFORM proxima_core.owner_rls_apply_class(
        'install_owner_rls_table', format('class %s', class),
        target_schema, target_table, class, owner_column);
    PERFORM proxima_core.owner_rls_census_table(target_schema, target_table);
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.install_owner_rls_table(target_schema text, target_table text, class proxima_core.owner_rls_class, owner_column text) FROM PUBLIC;
COMMENT ON FUNCTION proxima_core.install_owner_rls_table(target_schema text, target_table text, class proxima_core.owner_rls_class, owner_column text) IS 'Owner-RLS policies for one table of a flavor or host schema; the per-table form of install_owner_rls. A table in proxima_core.owner_follows gets the host follows rule instead of class owner_id or memory_owner (docs/build-a-flavor.md §2, docs/host-integration.md §Boot a composed host).';

-- Host only: record the Follows tables of the schemas this binary composes
-- and install their owner RLS. The host calls it from its Rust registry
-- (pg_sidecar!(owner: follows)) in the migration lane: once before the
-- flavor sources, when a table may not exist yet and is only marked, and once
-- after them. Marks of other schemas are left alone, so binaries composing
-- different bundles over one database do not undo each other.
CREATE OR REPLACE FUNCTION proxima_core.sync_owner_follows(schemas text[], tables text[])
 RETURNS void
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    listed text;
    rel_schema text;
    rel_name text;
    rel_oid oid;
    owner_attnum smallint;
    index_name text;
BEGIN
    IF current_setting('app.proxima_scope', true) IS DISTINCT FROM 'platform' THEN
        RAISE EXCEPTION 'sync_owner_follows: runs in platform scope only';
    END IF;
    IF schemas IS NULL OR tables IS NULL THEN
        RAISE EXCEPTION 'sync_owner_follows: schemas and tables must not be NULL';
    END IF;
    FOREACH rel_schema IN ARRAY schemas LOOP
        IF rel_schema IS NULL OR rel_schema IN ('proxima_core', 'public', 'information_schema')
           OR rel_schema LIKE 'pg\_%' THEN
            RAISE EXCEPTION 'sync_owner_follows: % is not a flavor schema', COALESCE(rel_schema, 'NULL');
        END IF;
    END LOOP;
    FOREACH listed IN ARRAY tables LOOP
        IF listed IS NULL OR listed <> lower(listed) OR listed !~ '^[^.]+\.[^.]+$' THEN
            RAISE EXCEPTION 'sync_owner_follows: % is not a lowercase schema.table name', COALESCE(listed, 'NULL');
        END IF;
        IF split_part(listed, '.', 1) <> ALL(schemas) THEN
            RAISE EXCEPTION 'sync_owner_follows: % is outside the composed schemas %', listed, schemas;
        END IF;
    END LOOP;

    DELETE FROM proxima_core.owner_follows AS f
     WHERE split_part(f.table_name, '.', 1) = ANY(schemas)
       AND f.table_name <> ALL(tables);
    INSERT INTO proxima_core.owner_follows (table_name)
    SELECT DISTINCT unnest(tables)
    ON CONFLICT DO NOTHING;

    FOR listed IN SELECT DISTINCT unnest(tables) ORDER BY 1 LOOP
        rel_schema := split_part(listed, '.', 1);
        rel_name := split_part(listed, '.', 2);
        SELECT c.oid INTO rel_oid
          FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = rel_schema AND c.relname = rel_name AND c.relkind IN ('r', 'p');
        -- Not created yet: the flavor lane creates it, its installer run
        -- applies the follows rule, and the post-lane sync adds the index.
        CONTINUE WHEN rel_oid IS NULL;

        PERFORM proxima_core.owner_rls_apply_follows('sync_owner_follows', rel_schema, rel_name);

        SELECT a.attnum INTO owner_attnum
          FROM pg_attribute AS a
         WHERE a.attrelid = rel_oid AND a.attname = 'owner_id' AND NOT a.attisdropped AND a.attnum > 0;
        IF NOT EXISTS (
            SELECT 1 FROM pg_index AS i
              JOIN pg_class AS ic ON ic.oid = i.indexrelid
              JOIN pg_am AS am ON am.oid = ic.relam
             WHERE i.indrelid = rel_oid AND i.indisvalid AND am.amname = 'btree'
               AND i.indkey[0] = owner_attnum
        ) THEN
            index_name := left(rel_name, 40) || '_owner_follows_idx';
            IF EXISTS (
                SELECT 1 FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
                 WHERE n.nspname = rel_schema AND c.relname = index_name
            ) THEN
                RAISE EXCEPTION 'sync_owner_follows: %.% exists but is no valid btree index leading with %.owner_id',
                    rel_schema, index_name, listed;
            END IF;
            EXECUTE format('CREATE INDEX %I ON %I.%I (owner_id)', index_name, rel_schema, rel_name);
        END IF;

        PERFORM proxima_core.owner_rls_census_table(rel_schema, rel_name);
    END LOOP;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.sync_owner_follows(schemas text[], tables text[]) FROM PUBLIC;
COMMENT ON FUNCTION proxima_core.sync_owner_follows(schemas text[], tables text[]) IS 'Host only: replaces the owner_follows marks of the composed schemas with the registry Follows tables, and gives each existing one the follows owner RLS (owner_id AND EXISTS over its memory) and a btree index leading with owner_id.';

-- Lexical functions

CREATE OR REPLACE FUNCTION proxima_core.lexical_language_forget(config_to_forget regconfig)
 RETURNS void
 LANGUAGE plpgsql
AS $function$
DECLARE
    holder_table text;
    fk_detail text;
BEGIN
    IF config_to_forget IS NULL THEN
        RAISE EXCEPTION 'lexical configuration must not be null';
    END IF;
    IF config_to_forget = proxima_core.lexical_config() THEN
        RAISE EXCEPTION 'cannot forget %: it is the default lexical configuration',
            config_to_forget;
    END IF;

    BEGIN
        DELETE FROM proxima_core.lexical_languages
         WHERE config = config_to_forget;
        -- Zero rows deleted = not registered: nothing to forget.
    EXCEPTION
        WHEN foreign_key_violation THEN
            GET STACKED DIAGNOSTICS
                holder_table = TABLE_NAME,
                fk_detail = PG_EXCEPTION_DETAIL;
            RAISE EXCEPTION 'cannot forget %: rows in % still reference it (%)',
                config_to_forget, holder_table, fk_detail
                USING ERRCODE = '23503';
    END;
END
$function$;
COMMENT ON FUNCTION proxima_core.lexical_language_forget(config_to_forget regconfig) IS 'Remove a configuration from the active-language set, refusing while any row still holds it in an FK-stamped lexical_language column. The FK checks serialize this against in-flight writes. Run this BEFORE dropping a custom text search configuration: PostgreSQL allows the drop with rows still referencing it, and those rows are then un-updatable (cache lookup failed on the dangling OID).';

CREATE OR REPLACE FUNCTION proxima_core.lexical_query_text(config regconfig, query_text text)
 RETURNS text
 LANGUAGE sql
 STABLE PARALLEL SAFE
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
    WITH query_source AS MATERIALIZED (
        SELECT segment[1] AS token, ordinal
          FROM regexp_matches(query_text, '([[:alnum:]]+|[^[:alnum:]]+)', 'g')
               WITH ORDINALITY AS parts(segment, ordinal)
    ), query_tokens AS MATERIALIZED (
        SELECT token, ordinal,
               sum(length(token) - length(replace(token, '"', '')))
                   OVER (ORDER BY ordinal) % 2 = 1 AS in_phrase,
               sum(length(token)) OVER (ORDER BY ordinal) AS end_position
          FROM query_source
    ), query_words AS MATERIALIZED (
        SELECT DISTINCT token
          FROM query_tokens
         WHERE token ~ '[[:alpha:]]'
    ), query_languages AS MATERIALIZED (
        SELECT l.config FROM proxima_core.lexical_languages l
        UNION ALL
        SELECT config WHERE NOT EXISTS (SELECT 1 FROM proxima_core.lexical_languages)
    ), query_stopwords AS MATERIALIZED (
        SELECT l.config, w.token
          FROM query_languages l
          CROSS JOIN query_words w
         WHERE EXISTS (
             SELECT 1 FROM pg_catalog.ts_debug(l.config, w.token) d
              WHERE d.token = w.token AND d.lexemes = ARRAY[]::text[]
         )
    ), query_stopword_counts AS (
        SELECT s.config, count(*) AS stopword_count
          FROM query_stopwords s
          JOIN query_tokens t USING (token)
         GROUP BY s.config
    ), query_dominant_languages AS (
        SELECT config FROM query_stopword_counts
         WHERE stopword_count = (SELECT max(stopword_count) FROM query_stopword_counts)
    ), query_removed_words AS MATERIALIZED (
        SELECT DISTINCT s.token FROM query_stopwords s
        JOIN query_dominant_languages l USING (config)
    ), query_or_probes AS MATERIALIZED (
        -- Let PostgreSQL's parser distinguish a binary OR from an ordinary word.
        -- A sentinel operand exposes parser state even for consecutive OR tokens.
        SELECT t.ordinal,
               websearch_to_tsquery('pg_catalog.simple'::regconfig,
                   left(query_text, t.end_position::integer) || ' proximaoperatorprobe')::text AS with_or,
               websearch_to_tsquery('pg_catalog.simple'::regconfig,
                   left(query_text, (t.end_position - length(t.token))::integer)
                       || ' proximaoperatorprobe')::text AS without_or
          FROM query_tokens t
         WHERE lower(t.token) = 'or' AND NOT t.in_phrase
           AND substr(query_text, (t.end_position + 1)::integer, 1) NOT IN ('-', '_')
           AND substr(query_text, (t.end_position + 2)::integer) ~ '[^[:space:]]'
    ), query_or_operators AS MATERIALIZED (
        SELECT ordinal FROM query_or_probes
         WHERE length(with_or) - length(replace(with_or, ' | ', ''))
             > length(without_or) - length(replace(without_or, ' | ', ''))
    ), query_removed_tokens AS MATERIALIZED (
        SELECT t.ordinal FROM query_tokens t
        JOIN query_removed_words w USING (token)
         WHERE NOT EXISTS (SELECT 1 FROM query_or_operators o WHERE o.ordinal = t.ordinal)
    ), query_operand_counts AS (
        SELECT t.ordinal,
               count(*) FILTER (WHERE t.token ~ '[[:alnum:]]'
                   AND removed.ordinal IS NULL AND o.ordinal IS NULL)
                   OVER (ORDER BY t.ordinal) AS operands_before,
               count(*) FILTER (WHERE t.token ~ '[[:alnum:]]'
                   AND removed.ordinal IS NULL AND o.ordinal IS NULL)
                   OVER () AS operands_total
          FROM query_tokens t
          LEFT JOIN query_removed_tokens removed USING (ordinal)
          LEFT JOIN query_or_operators o USING (ordinal)
    ), query_retained_or AS (
        -- Removed operands can leave a leading OR or a run of adjacent ORs.
        SELECT min(o.ordinal) AS ordinal FROM query_or_operators o
        JOIN query_operand_counts counts USING (ordinal)
         WHERE operands_before > 0 AND operands_before < operands_total
         GROUP BY operands_before
    ), query_rendered_tokens AS MATERIALIZED (
        SELECT t.ordinal,
               CASE WHEN EXISTS (
                   SELECT 1 FROM query_removed_tokens removed WHERE removed.ordinal = t.ordinal
               ) THEN ''
               WHEN EXISTS (SELECT 1 FROM query_removed_tokens)
                AND EXISTS (SELECT 1 FROM query_or_operators o WHERE o.ordinal = t.ordinal)
                AND NOT EXISTS (SELECT 1 FROM query_retained_or o WHERE o.ordinal = t.ordinal)
               THEN ''
               -- A removed negated word must not transfer its minus to the next word.
               WHEN right(t.token, 1) = '-' AND EXISTS (
                   SELECT 1 FROM query_removed_tokens removed
                    WHERE removed.ordinal = t.ordinal + 1
               ) AND strpos(websearch_to_tsquery('pg_catalog.simple'::regconfig,
                   left(query_text, t.end_position::integer)
                       || 'proximaoperatorprobe')::text, '!''proximaoperatorprobe''') > 0
               THEN rtrim(t.token, '-')
               ELSE t.token END AS token
          FROM query_tokens t
    )
    SELECT COALESCE(string_agg(
               -- A deleted compound prefix must not turn its following dash into NOT.
               CASE WHEN left(t.token, 1) = '-' AND EXISTS (
                   SELECT 1 FROM query_removed_tokens removed
                    WHERE removed.ordinal = t.ordinal - 1
               ) AND strpos(websearch_to_tsquery('pg_catalog.simple'::regconfig,
                   (SELECT string_agg(prefix.token, '' ORDER BY prefix.ordinal)
                      FROM query_rendered_tokens prefix WHERE prefix.ordinal <= t.ordinal)
                       || 'proximaoperatorprobe')::text, '!''proximaoperatorprobe''') > 0
               THEN ltrim(t.token, '-')
               ELSE t.token END,
               '' ORDER BY t.ordinal
           ), query_text)
      FROM query_rendered_tokens t
$function$;

CREATE OR REPLACE FUNCTION proxima_core.lexical_scrub(txt text)
 RETURNS text
 LANGUAGE sql
 IMMUTABLE PARALLEL SAFE STRICT
AS $function$ SELECT regexp_replace(
       regexp_replace(txt, '[[:punct:]]+', ' ', 'g'),
       '\m[[:alnum:]]{255}[[:alnum:]]+\M', ' ', 'g') $function$;

CREATE OR REPLACE FUNCTION proxima_core.lexical_tsv(txt text)
 RETURNS tsvector
 LANGUAGE sql
 STABLE PARALLEL SAFE STRICT
AS $function$ SELECT to_tsvector(proxima_core.lexical_config(), proxima_core.lexical_scrub(txt)) $function$;

CREATE OR REPLACE FUNCTION proxima_core.lexical_tsv(config regconfig, txt text)
 RETURNS tsvector
 LANGUAGE sql
 IMMUTABLE PARALLEL SAFE STRICT
AS $function$ SELECT to_tsvector(config, proxima_core.lexical_scrub(txt)) $function$;

CREATE OR REPLACE FUNCTION proxima_core.remember_lexical_language()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    INSERT INTO proxima_core.lexical_languages (config)
    VALUES (NEW.lexical_language)
    ON CONFLICT DO NOTHING;
    RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION proxima_core.set_lexical_config(cfg text)
 RETURNS void
 LANGUAGE sql
AS $function$ INSERT INTO proxima_core.lexical_languages (config)
   VALUES (cfg::regconfig)
   ON CONFLICT DO NOTHING;
   INSERT INTO proxima_core.lexical_default (singleton, config)
   VALUES (true, cfg::regconfig)
   ON CONFLICT (singleton) DO UPDATE
   SET config = EXCLUDED.config $function$;

-- Lock functions

CREATE OR REPLACE FUNCTION proxima_core.lock_group_tree(p_group uuid)
 RETURNS void
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    before_root uuid;
    after_root uuid;
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING
            ERRCODE = '25001',
            MESSAGE = 'nested group membership writes require READ COMMITTED';
    END IF;
    LOOP
        before_root := proxima_core.group_tree_root(p_group);
        before_root := COALESCE(before_root, p_group);
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-group-tree:' || before_root::text, 0));
        after_root := proxima_core.group_tree_root(p_group);
        after_root := COALESCE(after_root, p_group);
        EXIT WHEN before_root = after_root;
    END LOOP;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.lock_group_tree(p_group uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.lock_group_trees(p_left uuid, p_right uuid)
 RETURNS void
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    left_root uuid;
    right_root uuid;
    checked_left uuid;
    checked_right uuid;
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING ERRCODE = '25001',
            MESSAGE = 'nested group membership writes require READ COMMITTED';
    END IF;
    LOOP
        left_root := proxima_core.group_tree_root(p_left);
        right_root := proxima_core.group_tree_root(p_right);
        left_root := COALESCE(left_root, p_left);
        right_root := COALESCE(right_root, p_right);
        IF left_root <= right_root THEN
            PERFORM pg_advisory_xact_lock(hashtextextended('proxima-group-tree:' || left_root::text, 0));
            IF left_root <> right_root THEN
                PERFORM pg_advisory_xact_lock(hashtextextended('proxima-group-tree:' || right_root::text, 0));
            END IF;
        ELSE
            PERFORM pg_advisory_xact_lock(hashtextextended('proxima-group-tree:' || right_root::text, 0));
            PERFORM pg_advisory_xact_lock(hashtextextended('proxima-group-tree:' || left_root::text, 0));
        END IF;
        checked_left := proxima_core.group_tree_root(p_left);
        checked_right := proxima_core.group_tree_root(p_right);
        EXIT WHEN checked_left = left_root AND checked_right = right_root;
    END LOOP;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.lock_group_trees(p_left uuid, p_right uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.lock_pin_targets(targets uuid[])
 RETURNS void
 LANGUAGE plpgsql
AS $function$
DECLARE
    target uuid;
BEGIN
    FOR target IN
        SELECT DISTINCT id
          FROM unnest(COALESCE(targets, '{}'::uuid[])) AS pins(id)
         WHERE id IS NOT NULL
         ORDER BY id
    LOOP
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-forget:' || target::text, 0)
        );
    END LOOP;
END;
$function$;

-- Memory functions

CREATE OR REPLACE FUNCTION proxima_core.memory_align_head()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
DECLARE
    head_kind proxima_core.memory_kind;
    head_owner uuid;
    head_schema text;
BEGIN
    SELECT kind, owner_id, schema_id INTO head_kind, head_owner, head_schema
      FROM proxima_core.memory_head
     WHERE handle = NEW.handle;
    IF head_kind IS DISTINCT FROM NEW.kind
       OR head_owner IS DISTINCT FROM NEW.owner_id
       OR head_schema IS DISTINCT FROM NEW.schema_id THEN
        RAISE EXCEPTION 'memory kind/owner/schema must equal memory_head'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION proxima_core.memory_erase_witness()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_pin_targets(ARRAY[OLD.t]);
    -- Forget keeps the row (#488), so deleting a memory row is always an
    -- erase and always leaves a witness.
    PERFORM proxima_core.record_erased_pin_target(
        OLD.t, OLD.kind::text::proxima_core.pin_target_kind
    );
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN OLD;
END;
$function$;
REVOKE ALL ON FUNCTION proxima_core.memory_erase_witness() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.memory_head_t_only()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id THEN
        RAISE EXCEPTION 'memory_head is frozen except t and owner_id'
            USING ERRCODE = '25006';
    END IF;
    IF NEW.publication_sequence < OLD.publication_sequence THEN
        RAISE EXCEPTION 'memory_head.publication_sequence never decreases'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION proxima_core.memory_owner_or_append_only()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.t IS DISTINCT FROM OLD.t
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW."by" IS DISTINCT FROM OLD."by"
       OR NEW.via IS DISTINCT FROM OLD.via
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
       OR NEW.source_id IS DISTINCT FROM OLD.source_id
       OR NEW.ingest_key IS DISTINCT FROM OLD.ingest_key
       OR NEW.origins IS DISTINCT FROM OLD.origins
       OR NEW.refs IS DISTINCT FROM OLD.refs
       OR NEW.goal_refs IS DISTINCT FROM OLD.goal_refs
       OR NEW.occurred_at IS DISTINCT FROM OLD.occurred_at
       OR NEW.sidecar_tables IS DISTINCT FROM OLD.sidecar_tables THEN
        RAISE EXCEPTION 'append-only: % does not accept UPDATE', TG_TABLE_NAME
            USING ERRCODE = '25006';
    END IF;
    IF NEW.blob_id IS DISTINCT FROM OLD.blob_id
       AND NOT EXISTS (
               SELECT 1
                 FROM proxima_core.blob old_blob
                 JOIN proxima_core.blob new_blob
                   ON new_blob.schema_id = old_blob.schema_id
                  AND new_blob.content_hash = old_blob.content_hash
                WHERE old_blob.blob_id = OLD.blob_id
                  AND new_blob.blob_id = NEW.blob_id
           ) THEN
        RAISE EXCEPTION
            'append-only: %.blob_id may only be repointed at a blob row naming the same '
            'schema_id and content_hash', TG_TABLE_NAME
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$function$;

-- FORGET-POLICY: includes-forgotten (pin integrity holds for forgotten targets) applies to every read in this function
CREATE OR REPLACE FUNCTION proxima_core.memory_pin_checks()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
    pin uuid;
    pin_handle uuid;
    pin_targets uuid[] := ARRAY[NEW.t] || NEW.origins || NEW.refs || NEW.goal_refs;
    pin_lock_mode text := current_setting('proxima_core.pin_lock_mode', true);
    pin_lock_count integer;
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    -- Until decided, the setting counts the target locks this transaction
    -- asked for here. Past 256, one pg_locks read decides the rest of the
    -- transaction; it scans the whole lock table, so a small write never pays
    -- it. 'coarse': memory and goal are both held in SHARE ROW
    -- EXCLUSIVE or stronger, which excludes every writer a target lock waits
    -- for. Anything else is 'fine'.
    IF pin_lock_mode IS DISTINCT FROM 'coarse'
       AND pin_lock_mode IS DISTINCT FROM 'fine'
    THEN
        pin_lock_count := COALESCE(
            substring(pin_lock_mode FROM '^[0-9]{1,6}$')::integer, 0
        ) + cardinality(pin_targets);
        IF pin_lock_count <= 256 THEN
            pin_lock_mode := pin_lock_count::text;
        ELSE
            SELECT CASE WHEN count(DISTINCT l.relation) = 2 THEN 'coarse' ELSE 'fine' END
              INTO pin_lock_mode
              FROM pg_catalog.pg_locks l
             WHERE l.locktype = 'relation'
               AND l.pid = pg_catalog.pg_backend_pid()
               AND l.granted
               AND l.mode IN ('ShareRowExclusiveLock', 'ExclusiveLock', 'AccessExclusiveLock')
               AND l.relation IN ('proxima_core.memory'::regclass,
                                  'proxima_core.goal'::regclass);
        END IF;
        PERFORM set_config('proxima_core.pin_lock_mode', pin_lock_mode, true);
    END IF;
    -- Take the table locks again instead of trusting the setting: held, this
    -- is a local no-op; a setting bound without them acquires them before any
    -- check below reads.
    IF pin_lock_mode = 'coarse' THEN
        LOCK TABLE proxima_core.memory, proxima_core.goal
            IN SHARE ROW EXCLUSIVE MODE;
    ELSE
        PERFORM proxima_core.lock_pin_targets(pin_targets);
    END IF;

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

    IF NEW.kind = 'fact'
       AND NEW.origins = '{}'
       AND NEW.refs = '{}'
       AND NEW.goal_refs = '{}'
    THEN
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;

    -- Goal references never provide F/A/P grounding.
    IF NEW.kind <> 'fact'
       AND NOT proxima_core.pins_have_grounding_support(
             NEW.origins || NEW.refs, NULL, NULL
           )
    THEN
        RAISE EXCEPTION 'non-fact must pin a memory'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.origins = '{}' AND NEW.refs = '{}' AND NEW.goal_refs = '{}' THEN
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;

    -- Origins are always Memory targets; a witness never satisfies them.
    SELECT p.id INTO pin
      FROM unnest(NEW.origins) AS p(id)
      LEFT JOIN proxima_core.memory m ON m.t = p.id
     WHERE m.t IS NULL
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'origin pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    -- `refs` carries only Memory targets after the 0004 split.
    SELECT p.id INTO pin
      FROM unnest(NEW.refs) AS p(id)
      LEFT JOIN proxima_core.memory m ON m.t = p.id
     WHERE m.t IS NULL
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'reference pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    -- `goal_refs` carries only Goal targets.
    SELECT p.id INTO pin
      FROM unnest(NEW.goal_refs) AS p(id)
      LEFT JOIN proxima_core.goal g ON g.t = p.id
     WHERE g.t IS NULL
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'goal reference pin % does not exist as a Goal', pin
            USING ERRCODE = '23503';
    END IF;

    SELECT m.handle INTO pin_handle
      FROM proxima_core.memory m
      JOIN proxima_core.closed_handle c ON c.handle = m.handle
     WHERE m.t = ANY (NEW.origins || NEW.refs)
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'closed_handle: no new pin to %', pin_handle
            USING ERRCODE = '23514';
    END IF;

    IF NEW.kind = 'abstraction' AND NEW.origins <> '{}' THEN
        IF EXISTS (
            SELECT 1
              FROM unnest(NEW.origins) AS o(id)
             WHERE NOT EXISTS (
                       SELECT 1 FROM proxima_core.memory m
                        WHERE m.t = o.id AND m.kind IN ('fact', 'abstraction')
                   )
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
        ) THEN
            RAISE EXCEPTION 'perspective origins must be abstraction t'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$function$;
REVOKE ALL ON FUNCTION proxima_core.memory_pin_checks() FROM PUBLIC;

-- Owner-RLS functions

CREATE OR REPLACE FUNCTION proxima_core.owner_group_kind_guard()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
    existing_kind proxima_core.owner_kind;
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    -- Personal registration is the v0.0.30 first-seen path.  The owners
    -- primary key and immutable-owner trigger already serialize it; taking a
    -- per-owner advisory lock here would add one lock to every memory write.
    IF NEW.kind = 'personal' THEN
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;
    IF NEW.kind = 'group' THEN
        PERFORM proxima_core.lock_group_tree(NEW.owner_id);
    END IF;
    PERFORM pg_advisory_xact_lock(
        hashtextextended('proxima-owner-kind:' || NEW.owner_id::text, 0));
    SELECT kind INTO existing_kind
      FROM proxima_core.owners
     WHERE owner_id = NEW.owner_id;
    IF FOUND THEN
        -- Registration is idempotent.  The owners immutability trigger owns
        -- UPDATE rejection; INSERT ... ON CONFLICT DO NOTHING must be able
        -- to propose either kind without changing an existing registration.
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;
    IF NEW.kind = 'group' AND EXISTS (
        SELECT 1 FROM proxima_core.group_memberships WHERE member_user_id = NEW.owner_id
    ) THEN
        RAISE EXCEPTION 'owner % cannot become a group while it has person memberships', NEW.owner_id;
    END IF;
    IF NEW.kind = 'personal' AND EXISTS (
        SELECT 1 FROM proxima_core.group_edges WHERE child_group_id = NEW.owner_id
    ) THEN
        RAISE EXCEPTION 'owner % cannot become a person while it is a group member', NEW.owner_id;
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.owner_group_kind_guard() FROM PUBLIC;

-- Generate SQL for an unaliased, schema-qualified target relation.
-- Parent queries retain their ordinary RLS; generation does not bind a scope.
-- FORGET-POLICY: live-only (policy subqueries run under owner RLS) applies to every read in this function
CREATE OR REPLACE FUNCTION proxima_core.owner_rls_class_predicates(caller text, class_label text, target_schema text, target_table text, class proxima_core.owner_rls_class, owner_column text)
 RETURNS TABLE(read_expr text, write_expr text, select_expr text)
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    rel_oid oid;
    fk jsonb;
    fk_child text;
    fk_schema text;
    fk_table text;
    fk_column text;
    parent_has_t boolean;
    memory_key text;
    memory_keys integer;
    memory_on_key boolean;
    direction text;
    fact_owners text;
    abstraction_owners text;
    perspective_owners text;
    goal_owners text;
    kind_of text;
BEGIN
    SELECT c.oid INTO rel_oid
      FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
     WHERE n.nspname = target_schema AND c.relname = target_table AND c.relkind IN ('r', 'p');

    CASE class
        WHEN 'owner_id' THEN
            memory_key := CASE WHEN target_schema = 'proxima_core' THEN
                CASE target_table
                    WHEN 'projection' THEN 'memory_id'
                    WHEN 'embeddings' THEN 'entity_id'
                    WHEN 'embedding_heads' THEN 'entity_id'
                    WHEN 'embedding_jobs' THEN 'entity_id'
                    WHEN 'vector_index_debt' THEN 'entity_id'
                END
            END;
            IF memory_key IS NOT NULL THEN
                IF owner_column IS DISTINCT FROM 'owner_id' THEN
                    RAISE EXCEPTION '%: %.% requires owner column owner_id',
                        caller, target_schema, target_table;
                END IF;
                FOREACH direction IN ARRAY ARRAY['read', 'write'] LOOP
                    fact_owners := format('(SELECT COALESCE(NULLIF(current_setting(%L, true), %L)::uuid[], %L::uuid[]))::uuid[]',
                        CASE direction WHEN 'read' THEN 'app.owner' ELSE 'app.write_owner' END, '', '{}');
                    abstraction_owners := format('(SELECT COALESCE(NULLIF(current_setting(%L, true), %L)::uuid[], %L::uuid[]))::uuid[]',
                        'app.' || direction || '_abstraction', '', '{}');
                    perspective_owners := format('(SELECT COALESCE(NULLIF(current_setting(%L, true), %L)::uuid[], %L::uuid[]))::uuid[]',
                        'app.' || direction || '_perspective', '', '{}');
                    goal_owners := format('(SELECT COALESCE(NULLIF(current_setting(%L, true), %L)::uuid[], %L::uuid[]))::uuid[]',
                        'app.' || direction || '_goal', '', '{}');
                    kind_of := format('CASE parent.kind::text WHEN ''fact'' THEN %s WHEN ''abstraction'' THEN %s WHEN ''perspective'' THEN %s ELSE %s END',
                        fact_owners, abstraction_owners, perspective_owners, goal_owners);
                    kind_of := format($expr$(
                        %1$I.%2$I.%3$I = ANY(%4$s)
                        OR (
                            %1$I.%2$I.%3$I = ANY(%5$s)
                            AND EXISTS (
                                SELECT 1 FROM proxima_core.memory AS parent
                                 WHERE parent.t = %1$I.%2$I.%6$I
                                   AND parent.owner_id = ANY(%7$s)
                            )
                        )
                    )$expr$, target_schema, target_table, owner_column,
                        goal_owners, fact_owners, memory_key, kind_of);
                    IF direction = 'read' THEN
                        read_expr := kind_of;
                    ELSE
                        write_expr := kind_of;
                    END IF;
                END LOOP;
            ELSE
                read_expr := format($expr$%I.%I.%I = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$, target_schema, target_table, owner_column);
                write_expr := format($expr$%I.%I.%I = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$, target_schema, target_table, owner_column);
            END IF;
        WHEN 'memory_owner' THEN
            -- The memory this row belongs to: the FK on the leading
            -- primary-key column, else the only FK to memory when no other t
            -- column competes, else t.
            SELECT child_att.attname, count(*) OVER (),
                   COALESCE(con.conkey[1] = pk.conkey[1], false)
              INTO memory_key, memory_keys, memory_on_key
              FROM pg_constraint AS con
              JOIN pg_attribute AS child_att ON child_att.attrelid = con.conrelid AND child_att.attnum = con.conkey[1]
              LEFT JOIN pg_constraint AS pk ON pk.conrelid = con.conrelid AND pk.contype = 'p'
             WHERE con.conrelid = rel_oid AND con.contype = 'f'
               AND con.confrelid = 'proxima_core.memory'::regclass
               AND array_length(con.conkey, 1) = 1
             ORDER BY COALESCE(con.conkey[1] = pk.conkey[1], false) DESC, con.oid
             LIMIT 1;
            IF memory_key IS NOT NULL AND NOT memory_on_key
               AND (memory_keys > 1 OR (memory_key <> 't' AND EXISTS (
                    SELECT 1 FROM pg_attribute
                     WHERE attrelid = rel_oid AND attname = 't' AND attnum > 0 AND NOT attisdropped)))
            THEN
                RAISE EXCEPTION '%: %.% has % foreign key(s) to proxima_core.memory, none on its leading primary-key column, beside a t column or each other; its memory is ambiguous',
                    caller, target_schema, target_table, memory_keys;
            END IF;
            memory_key := COALESCE(memory_key, 't');
            IF NOT EXISTS (
                SELECT 1 FROM pg_attribute
                 WHERE attrelid = rel_oid AND attname = memory_key AND atttypid = 'uuid'::regtype
                   AND attnum > 0 AND NOT attisdropped
            ) THEN
                RAISE EXCEPTION '%: %.% is % but has no uuid t column',
                    caller, target_schema, target_table, class_label;
            END IF;
            -- The parent memory's kind picks the list, read and write alike.
            FOREACH direction IN ARRAY ARRAY['read', 'write'] LOOP
                kind_of := format('CASE parent.kind::text WHEN ''fact'' THEN %s WHEN ''abstraction'' THEN %s ELSE %s END',
                    format('(SELECT COALESCE(NULLIF(current_setting(%L, true), %L)::uuid[], %L::uuid[]))::uuid[]',
                           CASE direction WHEN 'read' THEN 'app.owner' ELSE 'app.write_owner' END, '', '{}'),
                    format('(SELECT COALESCE(NULLIF(current_setting(%L, true), %L)::uuid[], %L::uuid[]))::uuid[]',
                           'app.' || direction || '_abstraction', '', '{}'),
                    format('(SELECT COALESCE(NULLIF(current_setting(%L, true), %L)::uuid[], %L::uuid[]))::uuid[]',
                           'app.' || direction || '_perspective', '', '{}'));
                kind_of := format('EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = %I.%I.%I AND parent.owner_id = ANY(%s))',
                    target_schema, target_table, memory_key, kind_of);
                IF direction = 'read' THEN
                    read_expr := kind_of;
                ELSE
                    write_expr := kind_of;
                END IF;
            END LOOP;
        WHEN 'ownerless' THEN
            read_expr := 'false';
            write_expr := 'false';
        WHEN 'fk_parent' THEN
            fk := proxima_core.owner_rls_parent_fk(caller, target_schema, target_table);
            fk_child := fk ->> 'child_column';
            fk_schema := fk ->> 'parent_schema';
            fk_table := fk ->> 'parent_table';
            fk_column := fk ->> 'parent_column';
            SELECT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid = format('%I.%I', fk_schema, fk_table)::regclass AND attname = 't' AND attnum > 0 AND NOT attisdropped) INTO parent_has_t;
            read_expr := format('EXISTS (SELECT 1 FROM %I.%I AS parent WHERE parent.%I = %I.%I.%I AND current_setting(''app.proxima_scope'', true) = ''owner'')', fk_schema, fk_table, fk_column, target_schema, target_table, fk_child);
            IF fk_schema = 'proxima_core' AND fk_table = 'memory' THEN
                write_expr := format('EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.%I = %I.%I.%I AND ((parent.kind = ''fact'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''abstraction'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''perspective'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))))', fk_column, target_schema, target_table, fk_child);
            ELSIF fk_schema = 'proxima_core' AND fk_table = 'goal' THEN
                write_expr := format('EXISTS (SELECT 1 FROM proxima_core.goal AS parent WHERE parent.%I = %I.%I.%I AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_goal'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))', fk_column, target_schema, target_table, fk_child);
            ELSIF parent_has_t THEN
                write_expr := format('EXISTS (SELECT 1 FROM %I.%I AS parent JOIN proxima_core.memory AS owner_memory ON owner_memory.t = parent.t WHERE parent.%I = %I.%I.%I AND ((owner_memory.kind = ''fact'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (owner_memory.kind = ''abstraction'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (owner_memory.kind = ''perspective'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))))', fk_schema, fk_table, fk_column, target_schema, target_table, fk_child);
            ELSE
                RAISE EXCEPTION 'owner RLS writable classification missing for %.%', target_schema, target_table;
            END IF;
    END CASE;

    select_expr := format('(%s) OR (%s)', read_expr, write_expr);
    RETURN NEXT;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.owner_rls_class_predicates(caller text, class_label text, target_schema text, target_table text, class proxima_core.owner_rls_class, owner_column text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.owner_rls_apply_class(caller text, class_label text, target_schema text, target_table text, class proxima_core.owner_rls_class, owner_column text)
 RETURNS void
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    read_expr text;
    write_expr text;
BEGIN
    SELECT predicates.read_expr, predicates.write_expr
      INTO read_expr, write_expr
      FROM proxima_core.owner_rls_class_predicates(
          caller, class_label, target_schema, target_table, class, owner_column
      ) AS predicates;
    PERFORM proxima_core.owner_rls_apply_policies(target_schema, target_table, read_expr, write_expr);
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.owner_rls_apply_class(caller text, class_label text, target_schema text, target_table text, class proxima_core.owner_rls_class, owner_column text) FROM PUBLIC;

-- The three owner-RLS policies of one table from its read and write
-- predicates, with ENABLE and FORCE: the one place they are created.
CREATE OR REPLACE FUNCTION proxima_core.owner_rls_apply_policies(target_schema text, target_table text, read_expr text, write_expr text)
 RETURNS void
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    rel_owner name;
BEGIN
    SELECT pg_get_userbyid(c.relowner) INTO rel_owner
      FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
     WHERE n.nspname = target_schema AND c.relname = target_table AND c.relkind IN ('r', 'p');

    EXECUTE format('ALTER TABLE %I.%I ENABLE ROW LEVEL SECURITY', target_schema, target_table);
    EXECUTE format('ALTER TABLE %I.%I FORCE ROW LEVEL SECURITY', target_schema, target_table);
    EXECUTE format('DROP POLICY IF EXISTS proxima_owner_read ON %I.%I', target_schema, target_table);
    EXECUTE format('DROP POLICY IF EXISTS proxima_owner_write ON %I.%I', target_schema, target_table);
    EXECUTE format('DROP POLICY IF EXISTS proxima_platform ON %I.%I', target_schema, target_table);
    EXECUTE format('CREATE POLICY proxima_owner_read ON %I.%I FOR SELECT TO PUBLIC USING (%s)', target_schema, target_table, read_expr);
    EXECUTE format('CREATE POLICY proxima_owner_write ON %I.%I FOR ALL TO PUBLIC USING (%s) WITH CHECK (%s)', target_schema, target_table, write_expr, write_expr);
    EXECUTE format('CREATE POLICY proxima_platform ON %I.%I FOR ALL TO %I USING (current_setting(''app.proxima_scope'', true) = ''platform'') WITH CHECK (current_setting(''app.proxima_scope'', true) = ''platform'')', target_schema, target_table, rel_owner);
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.owner_rls_apply_policies(target_schema text, target_table text, read_expr text, write_expr text) FROM PUBLIC;

-- The host's follows rule for a pg_sidecar!(owner: follows) table: the
-- owner_id class predicate AND the memory_owner class predicate, read and
-- write alike. The memory half reads through the row's memory under that
-- memory's own policies, so a forgotten memory hides the row from owner
-- scopes and the lifecycle scope still reaches it; the owner_id half lets the
-- planner use an owner_id index. Follows keeps owner_id equal to the
-- memory's owner, so the AND admits exactly the rows memory_owner admits.
CREATE OR REPLACE FUNCTION proxima_core.owner_rls_follows_predicates(caller text, target_schema text, target_table text)
 RETURNS TABLE(read_expr text, write_expr text)
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    by_owner record;
    by_memory record;
BEGIN
    SELECT * INTO by_owner
      FROM proxima_core.owner_rls_class_predicates(
          caller, 'a Follows table', target_schema, target_table, 'owner_id', 'owner_id');
    SELECT * INTO by_memory
      FROM proxima_core.owner_rls_class_predicates(
          caller, 'a Follows table', target_schema, target_table, 'memory_owner', 'owner_id');
    read_expr := format('(%s) AND (%s)', by_owner.read_expr, by_memory.read_expr);
    write_expr := format('(%s) AND (%s)', by_owner.write_expr, by_memory.write_expr);
    RETURN NEXT;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.owner_rls_follows_predicates(caller text, target_schema text, target_table text) FROM PUBLIC;

-- Install the follows rule on one existing flavor table. The table must
-- carry a uuid owner_id column: freeze requires it of every Follows sidecar.
CREATE OR REPLACE FUNCTION proxima_core.owner_rls_apply_follows(caller text, target_schema text, target_table text)
 RETURNS void
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    read_expr text;
    write_expr text;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_attribute AS a
          JOIN pg_class AS c ON c.oid = a.attrelid
          JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = target_schema AND c.relname = target_table AND c.relkind IN ('r', 'p')
           AND a.attname = 'owner_id' AND a.atttypid = 'uuid'::regtype
           AND NOT a.attisdropped AND a.attnum > 0
    ) THEN
        RAISE EXCEPTION '%: Follows table %.% has no uuid owner_id column', caller, target_schema, target_table;
    END IF;
    SELECT predicates.read_expr, predicates.write_expr
      INTO read_expr, write_expr
      FROM proxima_core.owner_rls_follows_predicates(caller, target_schema, target_table) AS predicates;
    PERFORM proxima_core.owner_rls_apply_policies(target_schema, target_table, read_expr, write_expr);
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.owner_rls_apply_follows(caller text, target_schema text, target_table text) FROM PUBLIC;

-- Candidate policies are limited to derived projections, the local vector index
-- and the core sidecars the substring candidate function reads through their
-- trigram indexes.
CREATE OR REPLACE FUNCTION proxima_core.is_search_candidate_relation(relation_id oid)
 RETURNS boolean
 LANGUAGE sql STABLE
 SET search_path TO 'pg_catalog'
AS $function$
    SELECT EXISTS (
        SELECT 1 FROM pg_class relation JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
         WHERE relation.oid = relation_id AND relation.relkind IN ('r', 'p')
           AND ((namespace.nspname = 'proxima_core' AND relation.relname IN (
                    'embeddings', 'embedding_heads',
                    'agent_derivation_v1', 'agent_note_v1', 'interpretation_v1', 'utterance_v1'))
             OR (EXISTS (
                 SELECT 1 FROM pg_attribute a WHERE a.attrelid = relation_id AND a.attname = 'search_tsv'
                   AND a.atttypid = 'tsvector'::regtype AND a.attnum > 0 AND NOT a.attisdropped
             ) AND EXISTS (
                 SELECT 1 FROM pg_constraint fk
                   JOIN pg_attribute child ON child.attrelid = relation_id AND child.attname = 'memory_id'
                   JOIN pg_attribute parent ON parent.attrelid = fk.confrelid AND parent.attname = 't'
                  WHERE fk.conrelid = relation_id AND fk.contype = 'f'
                    AND fk.confrelid = 'proxima_core.memory'::regclass
                    AND fk.conkey = ARRAY[child.attnum] AND fk.confkey = ARRAY[parent.attnum]
                    AND child.atttypid = 'uuid'::regtype AND NOT child.attisdropped
             )))
    )
$function$;

CREATE OR REPLACE FUNCTION proxima_core.owner_rls_census_table(target_schema text, target_table text)
 RETURNS void
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    relation record;
    candidate_policy record;
    expected_policies integer := 3;
BEGIN
    SELECT c.oid, n.nspname AS schema_name, c.relname, c.relowner,
           c.relrowsecurity, c.relforcerowsecurity
      INTO relation
      FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
     WHERE n.nspname = target_schema AND c.relname = target_table AND c.relkind IN ('r', 'p');
    IF NOT relation.relrowsecurity OR NOT relation.relforcerowsecurity THEN
        RAISE EXCEPTION 'table %.% lacks ENABLE/FORCE RLS', relation.schema_name, relation.relname;
    END IF;
    SELECT * INTO candidate_policy FROM pg_policy
     WHERE polrelid = relation.oid AND polname = 'proxima_search_candidates';
    IF FOUND THEN
        IF NOT proxima_core.is_search_candidate_relation(relation.oid)
           OR candidate_policy.polcmd <> 'r' OR NOT candidate_policy.polpermissive
           OR candidate_policy.polroles IS DISTINCT FROM ARRAY[to_regrole('proxima_search')::oid]
           OR pg_get_expr(candidate_policy.polqual, relation.oid) IS DISTINCT FROM 'true'
           OR candidate_policy.polwithcheck IS NOT NULL
        THEN
            RAISE EXCEPTION 'table %.% has an invalid search candidate policy', relation.schema_name, relation.relname;
        END IF;
        expected_policies := 4;
    END IF;
    IF (SELECT count(*) FROM pg_policy WHERE polrelid = relation.oid) <> expected_policies THEN
        IF expected_policies = 3 THEN
            RAISE EXCEPTION 'table %.% does not have exactly three RLS policies', relation.schema_name, relation.relname;
        ELSE
            RAISE EXCEPTION 'table %.% has unexpected search RLS policies', relation.schema_name, relation.relname;
        END IF;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_policy WHERE polrelid = relation.oid AND polname = 'proxima_platform' AND polroles = ARRAY[relation.relowner] AND polcmd = '*') THEN
        RAISE EXCEPTION 'table %.% platform policy is not owner-role limited', relation.schema_name, relation.relname;
    END IF;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.owner_rls_census_table(target_schema text, target_table text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.owner_rls_find_parent_fk(target_schema text, target_table text)
 RETURNS jsonb
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    fk record;
BEGIN
    SELECT child_att.attname AS child_column,
           parent_ns.nspname AS parent_schema,
           parent.relname AS parent_table,
           parent_att.attname AS parent_column,
           COALESCE(con.conkey[1] = pk.conkey[1], false) AS on_key,
           count(*) OVER () AS candidates
      INTO fk
      FROM pg_constraint AS con
      JOIN pg_class AS child ON child.oid = con.conrelid
      JOIN pg_namespace AS child_ns ON child_ns.oid = child.relnamespace
      JOIN pg_attribute AS child_att ON child_att.attrelid = con.conrelid AND child_att.attnum = con.conkey[1]
      JOIN pg_class AS parent ON parent.oid = con.confrelid
      JOIN pg_namespace AS parent_ns ON parent_ns.oid = parent.relnamespace
      JOIN pg_attribute AS parent_att ON parent_att.attrelid = parent.oid AND parent_att.attnum = con.confkey[1]
      LEFT JOIN pg_constraint AS pk ON pk.conrelid = con.conrelid AND pk.contype = 'p'
     WHERE child_ns.nspname = target_schema AND child.relname = target_table
       AND child.relkind IN ('r', 'p') AND con.contype = 'f'
       AND array_length(con.conkey, 1) = 1 AND array_length(con.confkey, 1) = 1
       AND con.confrelid <> con.conrelid
       AND parent_ns.nspname IN ('proxima_core', target_schema)
       -- An FK to a partitioned parent adds one row per referenced
       -- partition, each a clone of this table's own parent-level FK;
       -- one a partition inherits from its partitioned table stays.
       AND NOT EXISTS (SELECT 1 FROM pg_constraint AS clone_of
                        WHERE clone_of.oid = con.conparentid AND clone_of.conrelid = con.conrelid)
     ORDER BY COALESCE(con.conkey[1] = pk.conkey[1], false) DESC,
              (parent_ns.nspname = 'proxima_core' AND parent.relname IN ('memory', 'goal')) DESC,
              con.oid
     LIMIT 1;
    IF fk IS NULL THEN
        RETURN NULL;
    END IF;
    RETURN to_jsonb(fk);
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.owner_rls_find_parent_fk(target_schema text, target_table text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.owner_rls_parent_fk(caller text, target_schema text, target_table text)
 RETURNS jsonb
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    fk jsonb := proxima_core.owner_rls_find_parent_fk(target_schema, target_table);
BEGIN
    IF fk IS NULL THEN
        RAISE EXCEPTION 'owner RLS classification missing for %.%: no single-column FK to proxima_core or %',
            target_schema, target_table, target_schema;
    END IF;
    IF NOT (fk ->> 'on_key')::boolean AND (fk ->> 'candidates')::integer > 1 THEN
        RAISE EXCEPTION '%: %.% has % candidate parent FKs and none on its leading primary-key column; its owner is ambiguous',
            caller, target_schema, target_table, fk ->> 'candidates';
    END IF;
    RETURN fk;
END
$function$;
REVOKE ALL ON FUNCTION proxima_core.owner_rls_parent_fk(caller text, target_schema text, target_table text) FROM PUBLIC;

-- Pin functions

-- FORGET-POLICY: includes-forgotten (grounding support counts forgotten sources) applies to every read in this function
CREATE OR REPLACE FUNCTION proxima_core.pins_have_grounding_support(pins uuid[], cooling uuid, cooling_kind proxima_core.memory_kind)
 RETURNS boolean
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
    supported boolean;
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    SELECT EXISTS (
        SELECT 1
          FROM unnest(pins) AS p(id)
         WHERE CASE
                 -- The memory being forgotten still grounds (#488 S13).
                 WHEN cooling IS NOT NULL AND p.id = cooling THEN
                   true
                 ELSE
                   -- Any memory row grounds, forgotten or not.
                   EXISTS (
                        SELECT 1 FROM proxima_core.memory h
                         WHERE h.t = p.id
                   )
               END
    ) INTO supported;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN supported;
END;
$function$;
REVOKE ALL ON FUNCTION proxima_core.pins_have_grounding_support(pins uuid[], cooling uuid, cooling_kind proxima_core.memory_kind) FROM PUBLIC;

-- Store identity functions

CREATE OR REPLACE FUNCTION proxima_core.enforce_store_identity_immutable()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'write-once: the store number cannot be changed or removed'
        USING ERRCODE = '25006';
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.memory_created_at(id uuid)
RETURNS timestamptz LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT TIMESTAMPTZ '1970-01-01 00:00:00+00'
        + (millis / 1000) * INTERVAL '1 second'
        + (millis % 1000) * INTERVAL '1 millisecond'
    FROM (SELECT (
        (get_byte(uuid_send(id), 0)::bigint << 40) |
        (get_byte(uuid_send(id), 1)::bigint << 32) |
        (get_byte(uuid_send(id), 2)::bigint << 24) |
        (get_byte(uuid_send(id), 3)::bigint << 16) |
        (get_byte(uuid_send(id), 4)::bigint << 8) |
        get_byte(uuid_send(id), 5)::bigint
    ) AS millis) AS timestamp_bits
$$;

-- The handle is frozen and the position never goes back.
CREATE OR REPLACE FUNCTION proxima_core.publication_counter_forward_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle THEN
        RAISE EXCEPTION 'publication_counter.handle is frozen'
            USING ERRCODE = '25006';
    END IF;
    IF NEW.sequence < OLD.sequence THEN
        RAISE EXCEPTION 'publication_counter.sequence never decreases'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

-- Aggregates

CREATE OR REPLACE AGGREGATE proxima_core.tsquery_or_agg(tsquery) (
    SFUNC = tsquery_or,
    STYPE = tsquery,
    COMBINEFUNC = tsquery_or,
    PARALLEL = safe
);

-- Memory read/write policies both require their base owner set before kind checks.
-- Candidates use that union; ordinary memory RLS remains the per-kind authority.
CREATE OR REPLACE FUNCTION proxima_core.search_owner_ids()
 RETURNS uuid[]
 LANGUAGE sql STABLE
 SET search_path TO 'pg_catalog'
AS $function$
    SELECT CASE WHEN current_setting('app.proxima_scope', true) = 'owner'
           THEN ARRAY(SELECT DISTINCT owner_id
                        FROM unnest(
                            COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}')
                         || COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}')
                        ) AS scoped(owner_id))
           ELSE '{}'::uuid[] END
$function$;

CREATE OR REPLACE FUNCTION proxima_core.assert_search_scope(requested_owners uuid[])
 RETURNS void
 LANGUAGE plpgsql STABLE
 SET search_path TO 'pg_catalog'
AS $function$
BEGIN
    IF current_setting('app.proxima_scope', true) IS DISTINCT FROM 'owner'
       OR NULLIF(current_setting('app.owner', true), '') IS NULL
       OR requested_owners IS NULL
       OR array_position(requested_owners, NULL) IS NOT NULL
       OR NOT requested_owners <@ proxima_core.search_owner_ids()
    THEN
        RAISE EXCEPTION 'search requires an authenticated owner scope'
            USING ERRCODE = '42501';
    END IF;
END
$function$;

-- Generated candidate functions: search_candidates.rs and vector_index.rs.
DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxima_core TO proxima_search;
GRANT SELECT ON proxima_core.agent_derivation_v1, proxima_core.agent_note_v1, proxima_core.interpretation_v1, proxima_core.lexical_default, proxima_core.lexical_languages, proxima_core.memory, proxima_core.memory_head, proxima_core.projection, proxima_core.utterance_v1 TO proxima_search;
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.projection;
CREATE POLICY proxima_search_candidates ON proxima_core.projection FOR SELECT TO proxima_search USING (true);
SET LOCAL ROLE proxima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxima_core.search_lexical_candidates(query_text text, candidate_limit bigint, requested_tags text[], since_time timestamptz, until_time timestamptz, before_id uuid, requested_owners uuid[], requested_schemas text[], rescue boolean, recency boolean, heads_only boolean, requested_kind text, tag_all boolean)
RETURNS TABLE(t uuid, schema_id text, lexical_score real)
LANGUAGE plpgsql SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
AS $candidate_function$
BEGIN
PERFORM proxima_core.assert_search_scope(requested_owners);
IF candidate_limit IS NULL OR candidate_limit < 1 OR candidate_limit > 1000 THEN
RAISE EXCEPTION 'candidate limit must be between 1 and 1000' USING ERRCODE = '22023';
END IF;
IF recency THEN
RETURN QUERY EXECUTE 'WITH analyzed_query AS MATERIALIZED (
             SELECT proxima_core.lexical_scrub($1) AS scrubbed,
                    proxima_core.lexical_query_text(proxima_core.lexical_config(),
                        proxima_core.lexical_scrub($1)) AS analyzed
         ), q AS MATERIALIZED (
             SELECT s.scrubbed, s.analyzed,
                    COALESCE(
                        (SELECT proxima_core.tsquery_or_agg(
                                    websearch_to_tsquery(l.config, s.analyzed)
                                    ORDER BY l.config)
                           FROM proxima_core.lexical_languages l),
                        websearch_to_tsquery(proxima_core.lexical_config(), s.analyzed)
                    ) AS tsq,
                    COALESCE(
                        (SELECT proxima_core.tsquery_or_agg(
                                    NULLIF(
                                        replace(
                                            plainto_tsquery(l.config, s.analyzed)::text,
                                            '' & '', '' | ''),
                                        '''')::tsquery
                                    ORDER BY l.config)
                           FROM proxima_core.lexical_languages l),
                        NULLIF(
                            replace(
                                plainto_tsquery(proxima_core.lexical_config(), s.analyzed)::text,
                                '' & '', '' | ''),
                            '''')::tsquery
                    ) AS any_tsq
               FROM analyzed_query s
         )
         SELECT p.memory_id AS t,
                p.schema_id,
                GREATEST(
                    CASE WHEN p.search_tsv @@ q.tsq
                         THEN 0.50 + LEAST(COALESCE(ts_rank_cd(p.search_tsv, websearch_to_tsquery(p.lexical_language, q.analyzed), 32), 0.0), 1.0) * 0.50
                         ELSE 0.0 END, CASE WHEN $9::boolean AND q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq
                    THEN 0.25 + LEAST(COALESCE(ts_rank(p.search_tsv, q.any_tsq, 33), 0.0) * 100.0, 1.0) * 0.20
                    ELSE 0.0 END,
                    0.0
                )::real AS lexical_score
           FROM proxima_core.projection p, q
          WHERE p.owner_id = ANY($7::uuid[])
            AND p.schema_id = ANY($8::text[])
            AND (p.search_tsv @@ q.tsq OR ($9::boolean AND q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq))
             AND p.owner_id = ANY(proxima_core.search_owner_ids()) AND ($3::text[] IS NULL OR CASE WHEN $12::boolean THEN p.tag @> $3::text[] ELSE p.tag && $3::text[] END) AND EXISTS (SELECT 1 FROM proxima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = p.memory_id AND authorized_memory.owner_id = ANY($7::uuid[]) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR EXISTS (SELECT 1 FROM proxima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t)))
            AND ($4::timestamptz IS NULL
                 OR proxima_core.memory_created_at(p.memory_id) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(p.memory_id) <= $5)
            AND ($6::uuid IS NULL OR p.memory_id < $6)
          ORDER BY p.memory_id DESC
          LIMIT $2' USING query_text, candidate_limit, requested_tags, since_time, until_time, before_id, requested_owners, requested_schemas, rescue, heads_only, requested_kind, tag_all;
ELSE
RETURN QUERY EXECUTE 'WITH analyzed_query AS MATERIALIZED (
             SELECT proxima_core.lexical_scrub($1) AS scrubbed,
                    proxima_core.lexical_query_text(proxima_core.lexical_config(),
                        proxima_core.lexical_scrub($1)) AS analyzed
         ), q AS MATERIALIZED (
             SELECT s.scrubbed, s.analyzed,
                    COALESCE(
                        (SELECT proxima_core.tsquery_or_agg(
                                    websearch_to_tsquery(l.config, s.analyzed)
                                    ORDER BY l.config)
                           FROM proxima_core.lexical_languages l),
                        websearch_to_tsquery(proxima_core.lexical_config(), s.analyzed)
                    ) AS tsq,
                    COALESCE(
                        (SELECT proxima_core.tsquery_or_agg(
                                    NULLIF(
                                        replace(
                                            plainto_tsquery(l.config, s.analyzed)::text,
                                            '' & '', '' | ''),
                                        '''')::tsquery
                                    ORDER BY l.config)
                           FROM proxima_core.lexical_languages l),
                        NULLIF(
                            replace(
                                plainto_tsquery(proxima_core.lexical_config(), s.analyzed)::text,
                                '' & '', '' | ''),
                            '''')::tsquery
                    ) AS any_tsq
               FROM analyzed_query s
         )
         SELECT p.memory_id AS t,
                p.schema_id,
                GREATEST(
                    CASE WHEN p.search_tsv @@ q.tsq
                         THEN 0.50 + LEAST(COALESCE(ts_rank_cd(p.search_tsv, websearch_to_tsquery(p.lexical_language, q.analyzed), 32), 0.0), 1.0) * 0.50
                         ELSE 0.0 END, CASE WHEN $9::boolean AND q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq
                    THEN 0.25 + LEAST(COALESCE(ts_rank(p.search_tsv, q.any_tsq, 33), 0.0) * 100.0, 1.0) * 0.20
                    ELSE 0.0 END,
                    0.0
                )::real AS lexical_score
           FROM proxima_core.projection p, q
          WHERE p.owner_id = ANY($7::uuid[])
            AND p.schema_id = ANY($8::text[])
            AND (p.search_tsv @@ q.tsq OR ($9::boolean AND q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq))
             AND p.owner_id = ANY(proxima_core.search_owner_ids()) AND ($3::text[] IS NULL OR CASE WHEN $12::boolean THEN p.tag @> $3::text[] ELSE p.tag && $3::text[] END) AND EXISTS (SELECT 1 FROM proxima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = p.memory_id AND authorized_memory.owner_id = ANY($7::uuid[]) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR EXISTS (SELECT 1 FROM proxima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t)))
            AND ($4::timestamptz IS NULL
                 OR proxima_core.memory_created_at(p.memory_id) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(p.memory_id) <= $5)
            AND ($6::uuid IS NULL OR p.memory_id < $6)
          ORDER BY lexical_score DESC, p.memory_id DESC
          LIMIT $2' USING query_text, candidate_limit, requested_tags, since_time, until_time, before_id, requested_owners, requested_schemas, rescue, heads_only, requested_kind, tag_all;
END IF;
END
$candidate_function$;$generated_function$;
REVOKE ALL ON FUNCTION proxima_core.search_lexical_candidates(text, bigint, text[], timestamp with time zone, timestamp with time zone, uuid, uuid[], text[], boolean, boolean, boolean, text, boolean) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxima_core FROM proxima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxima_core TO proxima_search;
GRANT SELECT ON proxima_core.agent_derivation_v1, proxima_core.agent_note_v1, proxima_core.interpretation_v1, proxima_core.lexical_default, proxima_core.lexical_languages, proxima_core.memory, proxima_core.memory_head, proxima_core.projection, proxima_core.utterance_v1 TO proxima_search;
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.projection;
CREATE POLICY proxima_search_candidates ON proxima_core.projection FOR SELECT TO proxima_search USING (true);
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.agent_derivation_v1;
CREATE POLICY proxima_search_candidates ON proxima_core.agent_derivation_v1 FOR SELECT TO proxima_search USING (true);
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.agent_note_v1;
CREATE POLICY proxima_search_candidates ON proxima_core.agent_note_v1 FOR SELECT TO proxima_search USING (true);
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.interpretation_v1;
CREATE POLICY proxima_search_candidates ON proxima_core.interpretation_v1 FOR SELECT TO proxima_search USING (true);
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.utterance_v1;
CREATE POLICY proxima_search_candidates ON proxima_core.utterance_v1 FOR SELECT TO proxima_search USING (true);
SET LOCAL ROLE proxima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxima_core.search_substring_candidates(query_text text, candidate_limit bigint, requested_tags text[], since_time timestamptz, until_time timestamptz, before_id uuid, requested_owners uuid[], requested_schemas text[], rescue boolean, recency boolean, heads_only boolean, requested_kind text, tag_all boolean)
RETURNS TABLE(t uuid, lexical_score real)
LANGUAGE plpgsql SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
AS $candidate_function$
BEGIN
PERFORM proxima_core.assert_search_scope(requested_owners);
IF candidate_limit IS NULL OR candidate_limit < 1 OR candidate_limit > 1000 THEN
RAISE EXCEPTION 'candidate limit must be between 1 and 1000' USING ERRCODE = '22023';
END IF;
IF recency THEN
DECLARE legs text[] := ARRAY[]::text[];
BEGIN
IF requested_schemas && ARRAY['core/agent-derivation-v1']::text[] THEN legs := legs || 'SELECT c.t AS t,
                0.25::real AS lexical_score
           FROM proxima_core.agent_derivation_v1 c
           JOIN proxima_core.memory m ON m.t = c.t /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
          WHERE m.owner_id = ANY($7::uuid[])
            AND m.schema_id = ANY($8::text[])
            AND m.schema_id = ANY(ARRAY[''core/agent-derivation-v1'']::text[])
            AND (lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.title, ''''), NULLIF(c.body, ''''), proxima_core.lexical_text_array(c.tags)])) LIKE $1 ESCAPE ''\'')
             AND ($3::text[] IS NULL OR CASE WHEN $12::boolean THEN c.tags @> $3::text[] ELSE c.tags && $3::text[] END) AND EXISTS (SELECT 1 FROM proxima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND authorized_memory.owner_id = ANY($7::uuid[]) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR EXISTS (SELECT 1 FROM proxima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/agent-derivation-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) <= $5)
            AND ($6::uuid IS NULL OR c.t < $6)'::text; END IF;
IF requested_schemas && ARRAY['core/agent-note-v1']::text[] THEN legs := legs || 'SELECT c.t AS t,
                0.25::real AS lexical_score
           FROM proxima_core.agent_note_v1 c
           JOIN proxima_core.memory m ON m.t = c.t /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
          WHERE m.owner_id = ANY($7::uuid[])
            AND m.schema_id = ANY($8::text[])
            AND m.schema_id = ANY(ARRAY[''core/agent-note-v1'']::text[])
            AND (lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.title, ''''), NULLIF(c.body, ''''), proxima_core.lexical_text_array(c.tags)])) LIKE $1 ESCAPE ''\'')
             AND ($3::text[] IS NULL OR CASE WHEN $12::boolean THEN c.tags @> $3::text[] ELSE c.tags && $3::text[] END) AND EXISTS (SELECT 1 FROM proxima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND authorized_memory.owner_id = ANY($7::uuid[]) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR EXISTS (SELECT 1 FROM proxima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/agent-note-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) <= $5)
            AND ($6::uuid IS NULL OR c.t < $6)'::text; END IF;
IF requested_schemas && ARRAY['core/interpretation-v1']::text[] THEN legs := legs || 'SELECT c.t AS t,
                0.25::real AS lexical_score
           FROM proxima_core.interpretation_v1 c
           JOIN proxima_core.memory m ON m.t = c.t /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
          WHERE m.owner_id = ANY($7::uuid[])
            AND m.schema_id = ANY($8::text[])
            AND m.schema_id = ANY(ARRAY[''core/interpretation-v1'']::text[])
            AND (lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.claim, '''')])) LIKE $1 ESCAPE ''\'')
             AND $3::text[] IS NULL AND EXISTS (SELECT 1 FROM proxima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND authorized_memory.owner_id = ANY($7::uuid[]) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR EXISTS (SELECT 1 FROM proxima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/interpretation-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) <= $5)
            AND ($6::uuid IS NULL OR c.t < $6)'::text; END IF;
IF requested_schemas && ARRAY['core/utterance-v1']::text[] THEN legs := legs || 'SELECT c.t AS t,
                0.25::real AS lexical_score
           FROM proxima_core.utterance_v1 c
           JOIN proxima_core.memory m ON m.t = c.t /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
          WHERE m.owner_id = ANY($7::uuid[])
            AND m.schema_id = ANY($8::text[])
            AND m.schema_id = ANY(ARRAY[''core/utterance-v1'']::text[])
            AND (lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.text, '''')])) LIKE $1 ESCAPE ''\'')
             AND $3::text[] IS NULL AND EXISTS (SELECT 1 FROM proxima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND authorized_memory.owner_id = ANY($7::uuid[]) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR EXISTS (SELECT 1 FROM proxima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/utterance-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) <= $5)
            AND ($6::uuid IS NULL OR c.t < $6)'::text; END IF;
IF cardinality(legs) = 0 THEN RETURN; END IF;
RETURN QUERY EXECUTE 'SELECT s.t, s.lexical_score FROM (' || array_to_string(legs, ' UNION ALL ') || ') s ORDER BY s.t DESC LIMIT $2' USING query_text, candidate_limit, requested_tags, since_time, until_time, before_id, requested_owners, requested_schemas, rescue, heads_only, requested_kind, tag_all;
END;
ELSE
DECLARE legs text[] := ARRAY[]::text[];
BEGIN
IF requested_schemas && ARRAY['core/agent-derivation-v1']::text[] THEN legs := legs || 'SELECT c.t AS t,
                0.25::real AS lexical_score
           FROM proxima_core.agent_derivation_v1 c
           JOIN proxima_core.memory m ON m.t = c.t /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
          WHERE m.owner_id = ANY($7::uuid[])
            AND m.schema_id = ANY($8::text[])
            AND m.schema_id = ANY(ARRAY[''core/agent-derivation-v1'']::text[])
            AND (lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.title, ''''), NULLIF(c.body, ''''), proxima_core.lexical_text_array(c.tags)])) LIKE $1 ESCAPE ''\'')
             AND ($3::text[] IS NULL OR CASE WHEN $12::boolean THEN c.tags @> $3::text[] ELSE c.tags && $3::text[] END) AND EXISTS (SELECT 1 FROM proxima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND authorized_memory.owner_id = ANY($7::uuid[]) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR EXISTS (SELECT 1 FROM proxima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/agent-derivation-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) <= $5)
            AND ($6::uuid IS NULL OR c.t < $6)'::text; END IF;
IF requested_schemas && ARRAY['core/agent-note-v1']::text[] THEN legs := legs || 'SELECT c.t AS t,
                0.25::real AS lexical_score
           FROM proxima_core.agent_note_v1 c
           JOIN proxima_core.memory m ON m.t = c.t /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
          WHERE m.owner_id = ANY($7::uuid[])
            AND m.schema_id = ANY($8::text[])
            AND m.schema_id = ANY(ARRAY[''core/agent-note-v1'']::text[])
            AND (lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.title, ''''), NULLIF(c.body, ''''), proxima_core.lexical_text_array(c.tags)])) LIKE $1 ESCAPE ''\'')
             AND ($3::text[] IS NULL OR CASE WHEN $12::boolean THEN c.tags @> $3::text[] ELSE c.tags && $3::text[] END) AND EXISTS (SELECT 1 FROM proxima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND authorized_memory.owner_id = ANY($7::uuid[]) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR EXISTS (SELECT 1 FROM proxima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/agent-note-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) <= $5)
            AND ($6::uuid IS NULL OR c.t < $6)'::text; END IF;
IF requested_schemas && ARRAY['core/interpretation-v1']::text[] THEN legs := legs || 'SELECT c.t AS t,
                0.25::real AS lexical_score
           FROM proxima_core.interpretation_v1 c
           JOIN proxima_core.memory m ON m.t = c.t /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
          WHERE m.owner_id = ANY($7::uuid[])
            AND m.schema_id = ANY($8::text[])
            AND m.schema_id = ANY(ARRAY[''core/interpretation-v1'']::text[])
            AND (lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.claim, '''')])) LIKE $1 ESCAPE ''\'')
             AND $3::text[] IS NULL AND EXISTS (SELECT 1 FROM proxima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND authorized_memory.owner_id = ANY($7::uuid[]) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR EXISTS (SELECT 1 FROM proxima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/interpretation-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) <= $5)
            AND ($6::uuid IS NULL OR c.t < $6)'::text; END IF;
IF requested_schemas && ARRAY['core/utterance-v1']::text[] THEN legs := legs || 'SELECT c.t AS t,
                0.25::real AS lexical_score
           FROM proxima_core.utterance_v1 c
           JOIN proxima_core.memory m ON m.t = c.t /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
          WHERE m.owner_id = ANY($7::uuid[])
            AND m.schema_id = ANY($8::text[])
            AND m.schema_id = ANY(ARRAY[''core/utterance-v1'']::text[])
            AND (lower(proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.text, '''')])) LIKE $1 ESCAPE ''\'')
             AND $3::text[] IS NULL AND EXISTS (SELECT 1 FROM proxima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND authorized_memory.owner_id = ANY($7::uuid[]) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR EXISTS (SELECT 1 FROM proxima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/utterance-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxima_core.memory_created_at(c.t) <= $5)
            AND ($6::uuid IS NULL OR c.t < $6)'::text; END IF;
IF cardinality(legs) = 0 THEN RETURN; END IF;
RETURN QUERY EXECUTE 'SELECT s.t, s.lexical_score FROM (' || array_to_string(legs, ' UNION ALL ') || ') s ORDER BY s.lexical_score DESC, s.t DESC LIMIT $2' USING query_text, candidate_limit, requested_tags, since_time, until_time, before_id, requested_owners, requested_schemas, rescue, heads_only, requested_kind, tag_all;
END;
END IF;
END
$candidate_function$;$generated_function$;
REVOKE ALL ON FUNCTION proxima_core.search_substring_candidates(text, bigint, text[], timestamp with time zone, timestamp with time zone, uuid, uuid[], text[], boolean, boolean, boolean, text, boolean) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxima_core FROM proxima_search;
END
$install_candidate$;
$rename_history_functions$::text WHERE false;
SELECT $rename_history_policies$-- Core row-level security policies. The installer replaces only the three policies declared for each table.
-- Every table is installed once through owner_rls_apply_class; exceptional expressions follow each call.

-- agent_derivation_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'agent_derivation_v1', 'fk_parent', NULL);

-- agent_note_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'agent_note_v1', 'fk_parent', NULL);

-- blob
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'blob', 'owner_id', 'owner_id');

-- blob_uploads
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'blob_uploads', 'owner_id', 'owner_id');

-- closed_handle
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'closed_handle', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.closed_handle USING ((EXISTS ( SELECT 1
   FROM proxima_core.memory_head parent
  WHERE ((parent.handle = closed_handle.handle) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END))))));

ALTER POLICY proxima_owner_write ON proxima_core.closed_handle USING ((EXISTS ( SELECT 1
   FROM proxima_core.memory_head parent
  WHERE ((parent.handle = closed_handle.handle) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END)))))) WITH CHECK ((EXISTS ( SELECT 1
   FROM proxima_core.memory_head parent
  WHERE ((parent.handle = closed_handle.handle) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END))))));

-- cold_purge_pending
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'cold_purge_pending', 'owner_id', 'owner_id');

-- content
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'content', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.content USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) OR ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND ((EXISTS ( SELECT 1
   FROM proxima_core.memory parent
  WHERE ((parent.content_id = content.content_id) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END)))))))));

ALTER POLICY proxima_owner_write ON proxima_core.content USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) OR ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND ((EXISTS ( SELECT 1
   FROM proxima_core.memory parent
  WHERE ((parent.content_id = content.content_id) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END))))))))) WITH CHECK ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

-- delegated_authority_grants
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'delegated_authority_grants', 'owner_id', 'owner_id');

-- embedding_heads
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'embedding_heads', 'owner_id', 'owner_id');

-- embedding_jobs
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'embedding_jobs', 'owner_id', 'owner_id');

-- embeddings
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'embeddings', 'owner_id', 'owner_id');

-- erased_pin_target
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'erased_pin_target', 'ownerless', NULL);

-- flavor_surface
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'flavor_surface', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.flavor_surface USING (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

-- goal
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'goal', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.goal USING ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

ALTER POLICY proxima_owner_write ON proxima_core.goal USING ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) WITH CHECK ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

-- goal_change_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'goal_change_v1', 'fk_parent', NULL);

-- goal_head
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'goal_head', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.goal_head USING ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

ALTER POLICY proxima_owner_write ON proxima_core.goal_head USING ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) WITH CHECK ((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

-- goal_replay_declaration
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'goal_replay_declaration', 'fk_parent', NULL);

-- group_edges
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'group_edges', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.group_edges USING (((child_group_id = ANY (COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) OR (parent_group_id = ANY (COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])))));

ALTER POLICY proxima_owner_write ON proxima_core.group_edges USING (((parent_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) OR (child_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))))) WITH CHECK (((parent_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) AND (child_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[])))));

-- group_lock_request
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'group_lock_request', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.group_lock_request USING (((left_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) OR (right_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[])))));

ALTER POLICY proxima_owner_write ON proxima_core.group_lock_request USING (((left_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) OR (right_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))))) WITH CHECK (((left_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))) OR (right_group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[])))));

-- group_memberships
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'group_memberships', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.group_memberships USING ((group_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

ALTER POLICY proxima_owner_write ON proxima_core.group_memberships USING ((group_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) WITH CHECK ((group_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])));

-- group_role_projection
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'group_role_projection', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.group_role_projection USING (
    group_id = ANY (COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))
    OR (member_user_id = NULLIF(current_setting('app.authority_subject', true), '')::uuid
        AND member_user_id = NULLIF(current_setting('app.subject', true), '')::uuid)
);

ALTER POLICY proxima_owner_write ON proxima_core.group_role_projection USING ((group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[])))) WITH CHECK ((group_id = ANY (COALESCE((NULLIF(current_setting('app.manage_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]))));

-- ingest_keys
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'ingest_keys', 'owner_id', 'owner_id');

-- installation
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'installation', 'ownerless', NULL);

-- interpretation_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'interpretation_v1', 'fk_parent', NULL);

-- lexical_default
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'lexical_default', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.lexical_default USING (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

-- lexical_languages
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'lexical_languages', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.lexical_languages USING (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

ALTER POLICY proxima_owner_write ON proxima_core.lexical_languages USING (false) WITH CHECK (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

-- memory
-- A forgotten Memory (forgotten_at set) is hidden from owner reads and
-- writes, and from every sidecar policy that reaches it through memory, unless
-- the transaction runs in the lifecycle scope (PgLifecycleScope, #488).
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'memory', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.memory USING ((((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (((kind = 'fact'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'abstraction'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'perspective'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))))) AND ((forgotten_at IS NULL) OR (current_setting('app.proxima_lifecycle'::text, true) = 'on'::text))));

ALTER POLICY proxima_owner_write ON proxima_core.memory USING ((((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (((kind = 'fact'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'abstraction'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'perspective'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))))) AND ((forgotten_at IS NULL) OR (current_setting('app.proxima_lifecycle'::text, true) = 'on'::text)))) WITH CHECK ((((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (((kind = 'fact'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'abstraction'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))) OR ((kind = 'perspective'::proxima_core.memory_kind) AND (owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[]))))) AND ((forgotten_at IS NULL) OR (current_setting('app.proxima_lifecycle'::text, true) = 'on'::text))));

-- memory_external_origin
-- Reached through the memory it describes (#478). Forget keeps the memory
-- row (#488), so a forgotten parent still has its owner; owner reads hide it
-- with the parent, lifecycle reads (export, erase, transfer) see it.
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class memory_owner', 'proxima_core', 'memory_external_origin', 'memory_owner', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.memory_external_origin USING (((EXISTS ( SELECT 1
   FROM proxima_core.memory parent
  WHERE ((parent.t = memory_external_origin.t) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END)))))));

ALTER POLICY proxima_owner_write ON proxima_core.memory_external_origin USING (((EXISTS ( SELECT 1
   FROM proxima_core.memory parent
  WHERE ((parent.t = memory_external_origin.t) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END))))))) WITH CHECK ((EXISTS ( SELECT 1
   FROM proxima_core.memory parent
  WHERE ((parent.t = memory_external_origin.t) AND (parent.owner_id = ANY (
        CASE (parent.kind)::text
            WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
            ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
        END))))));

-- memory_head
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'memory_head', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.memory_head USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END))));

ALTER POLICY proxima_owner_write ON proxima_core.memory_head USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END)))) WITH CHECK (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END))));

-- owner_rls_epoch
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'owner_rls_epoch', 'ownerless', NULL);

-- owners
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'owners', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.owners USING (
    owner_id = ANY (COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))
    OR (owner_id = NULLIF(current_setting('app.authority_subject', true), '')::uuid
        AND owner_id = NULLIF(current_setting('app.subject', true), '')::uuid)
);

-- projection
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'projection', 'owner_id', 'owner_id');

-- publication_consumer
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'publication_consumer', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.publication_consumer USING (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

-- publication_backlog: the publisher's estimate, read by every listenable
-- write whatever its owner, like publication_consumer.
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class ownerless', 'proxima_core', 'publication_backlog', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.publication_backlog USING (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

-- publication_delivery
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'publication_delivery', 'fk_parent', NULL);

-- publication_origin
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'publication_origin', 'owner_id', 'original_owner_id');

-- publication_outbox
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'publication_outbox', 'owner_id', 'owner_id');

-- sketch
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'sketch', 'owner_id', 'owner_id');

ALTER POLICY proxima_owner_read ON proxima_core.sketch USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.read_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.read_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END))));

ALTER POLICY proxima_owner_write ON proxima_core.sketch USING (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END)))) WITH CHECK (((owner_id = ANY (( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")::uuid[])) AND (owner_id = ANY (
CASE (kind)::text
    WHEN 'fact'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_owner'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'abstraction'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_abstraction'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    WHEN 'perspective'::text THEN ( SELECT COALESCE((NULLIF(current_setting('app.write_perspective'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
    ELSE ( SELECT COALESCE((NULLIF(current_setting('app.write_goal'::text, true), ''::text))::uuid[], '{}'::uuid[]) AS "coalesce")
END))));

-- source_cursors
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'source_cursors', 'owner_id', 'owner_id');

-- task_goal_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'task_goal_v1', 'fk_parent', NULL);

-- utterance_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'utterance_v1', 'fk_parent', NULL);

-- write_act_v1
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class fk_parent', 'proxima_core', 'write_act_v1', 'fk_parent', NULL);

-- store_identity
SELECT proxima_core.owner_rls_apply_class(
    'policies.sql', 'class ownerless', 'proxima_core', 'store_identity', 'ownerless', NULL);

-- publication_counter
SELECT proxima_core.owner_rls_apply_class(
    'policies.sql', 'class owner_id', 'proxima_core', 'publication_counter', 'owner_id', 'owner_id');

-- owner_follows: the host writes it (platform); owner scopes may read it,
-- like flavor_surface.
SELECT proxima_core.owner_rls_apply_class(
    'policies.sql', 'class ownerless', 'proxima_core', 'owner_follows', 'ownerless', NULL);

ALTER POLICY proxima_owner_read ON proxima_core.owner_follows USING (((current_setting('app.proxima_scope'::text, true) = 'owner'::text) AND (cardinality(COALESCE((NULLIF(current_setting('app.owner'::text, true), ''::text))::uuid[], '{}'::uuid[])) > 0)));

-- export_erase_fence and maintenance_lease: platform coordination state about
-- no owner. Only the platform scope reaches them.
SELECT proxima_core.owner_rls_apply_class(
    'policies.sql', 'class ownerless', 'proxima_core', 'export_erase_fence', 'ownerless', NULL);

SELECT proxima_core.owner_rls_apply_class(
    'policies.sql', 'class ownerless', 'proxima_core', 'maintenance_lease', 'ownerless', NULL);

-- vector_index_debt: read and written at its entity's kind, as its head is;
-- the platform scope drains it.
SELECT proxima_core.owner_rls_apply_class('policies.sql', 'class owner_id', 'proxima_core', 'vector_index_debt', 'owner_id', 'owner_id');
$rename_history_policies$::text WHERE false;
SELECT $rename_history_triggers$-- Core trigger definitions. The definitions pass replaces only the core triggers declared here.
-- Constraint triggers are dropped immediately before recreation; no drop-all pass is required.
-- Every trigger is grouped under its owning proxima_core table.

-- agent_derivation_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER agent_derivation_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.agent_derivation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS agent_derivation_v1_declared_by_memory_on_delete ON proxima_core.agent_derivation_v1;
CREATE CONSTRAINT TRIGGER agent_derivation_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.agent_derivation_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.agent_derivation_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER agent_derivation_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.agent_derivation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER agent_derivation_v1_append_only BEFORE UPDATE ON proxima_core.agent_derivation_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- agent_note_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER agent_note_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.agent_note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS agent_note_v1_declared_by_memory_on_delete ON proxima_core.agent_note_v1;
CREATE CONSTRAINT TRIGGER agent_note_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.agent_note_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.agent_note_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER agent_note_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.agent_note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER agent_note_v1_append_only BEFORE UPDATE ON proxima_core.agent_note_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- delegated_authority_grants
CREATE OR REPLACE TRIGGER delegated_authority_writer_immutable
    BEFORE UPDATE OF "by", via ON proxima_core.delegated_authority_grants
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- embedding_heads
CREATE OR REPLACE TRIGGER embedding_heads_deleted_index_debt
    AFTER DELETE ON proxima_core.embedding_heads
    REFERENCING OLD TABLE AS deleted_heads
    FOR EACH STATEMENT EXECUTE FUNCTION proxima_core.embedding_heads_deleted_index_debt();
CREATE OR REPLACE TRIGGER embedding_heads_moved_index_debt
    AFTER UPDATE OF owner_id ON proxima_core.embedding_heads
    FOR EACH ROW WHEN (OLD.owner_id IS DISTINCT FROM NEW.owner_id)
    EXECUTE FUNCTION proxima_core.embedding_head_moved_index_debt();


-- erased_pin_target
CREATE OR REPLACE TRIGGER erased_pin_target_append_only BEFORE DELETE OR UPDATE ON proxima_core.erased_pin_target FOR EACH ROW EXECUTE FUNCTION proxima_core.erased_pin_target_append_only();
CREATE OR REPLACE TRIGGER erased_pin_target_insert_guard BEFORE INSERT ON proxima_core.erased_pin_target FOR EACH ROW EXECUTE FUNCTION proxima_core.assert_erased_pin_target_insert();


-- goal
CREATE OR REPLACE TRIGGER goal_append_only BEFORE UPDATE ON proxima_core.goal FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE OR REPLACE TRIGGER goal_erased_pin_target BEFORE DELETE ON proxima_core.goal FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_erase_witness();
CREATE OR REPLACE TRIGGER goal_no_later_after_terminal BEFORE INSERT ON proxima_core.goal FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_no_later_after_terminal();
CREATE OR REPLACE TRIGGER goal_parent_fixed BEFORE INSERT ON proxima_core.goal FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_parent_fixed();
CREATE OR REPLACE TRIGGER goal_pin_target_checks BEFORE INSERT ON proxima_core.goal FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_pin_target_checks();


-- goal_change_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER goal_change_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.goal_change_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS goal_change_v1_declared_by_memory_on_delete ON proxima_core.goal_change_v1;
CREATE CONSTRAINT TRIGGER goal_change_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.goal_change_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.goal_change_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER goal_change_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.goal_change_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER goal_change_v1_append_only BEFORE UPDATE ON proxima_core.goal_change_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- goal_head
CREATE OR REPLACE TRIGGER goal_head_t_only BEFORE UPDATE ON proxima_core.goal_head FOR EACH ROW EXECUTE FUNCTION proxima_core.goal_head_t_only();


-- goal_replay_declaration
CREATE OR REPLACE TRIGGER goal_replay_declaration_append_only BEFORE UPDATE ON proxima_core.goal_replay_declaration FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- group_edges
CREATE OR REPLACE TRIGGER group_edge_before_write BEFORE INSERT OR DELETE OR UPDATE ON proxima_core.group_edges FOR EACH ROW EXECUTE FUNCTION proxima_core.group_edge_before_write();
CREATE OR REPLACE TRIGGER group_edge_projection AFTER INSERT OR DELETE OR UPDATE ON proxima_core.group_edges FOR EACH ROW EXECUTE FUNCTION proxima_core.group_projection_after_write();
CREATE OR REPLACE TRIGGER group_edge_truncate_guard BEFORE TRUNCATE ON proxima_core.group_edges FOR EACH STATEMENT EXECUTE FUNCTION proxima_core.group_truncate_guard();


-- group_lock_request
CREATE OR REPLACE TRIGGER group_lock_request_cleanup AFTER INSERT ON proxima_core.group_lock_request FOR EACH ROW EXECUTE FUNCTION proxima_core.group_lock_request_cleanup();
CREATE OR REPLACE TRIGGER group_lock_request_trigger BEFORE INSERT ON proxima_core.group_lock_request FOR EACH ROW EXECUTE FUNCTION proxima_core.group_lock_request_trigger();


-- group_memberships
CREATE OR REPLACE TRIGGER group_membership_before_write BEFORE INSERT OR DELETE OR UPDATE ON proxima_core.group_memberships FOR EACH ROW EXECUTE FUNCTION proxima_core.group_membership_before_write();
CREATE OR REPLACE TRIGGER group_membership_projection AFTER INSERT OR DELETE OR UPDATE ON proxima_core.group_memberships FOR EACH ROW EXECUTE FUNCTION proxima_core.group_projection_after_write();
CREATE OR REPLACE TRIGGER group_membership_truncate_guard BEFORE TRUNCATE ON proxima_core.group_memberships FOR EACH STATEMENT EXECUTE FUNCTION proxima_core.group_truncate_guard();


-- group_role_projection
CREATE OR REPLACE TRIGGER group_projection_truncate_guard BEFORE TRUNCATE ON proxima_core.group_role_projection FOR EACH STATEMENT EXECUTE FUNCTION proxima_core.group_truncate_guard();
CREATE OR REPLACE TRIGGER group_projection_write_guard BEFORE INSERT OR DELETE OR UPDATE ON proxima_core.group_role_projection FOR EACH ROW EXECUTE FUNCTION proxima_core.group_projection_write_guard();


-- ingest_keys
CREATE OR REPLACE TRIGGER ingest_keys_append_only BEFORE UPDATE ON proxima_core.ingest_keys FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- installation
CREATE OR REPLACE TRIGGER installation_identity_immutable BEFORE DELETE OR UPDATE ON proxima_core.installation FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_installation_immutable();


-- interpretation_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER interpretation_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.interpretation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS interpretation_v1_declared_by_memory_on_delete ON proxima_core.interpretation_v1;
CREATE CONSTRAINT TRIGGER interpretation_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.interpretation_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.interpretation_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER interpretation_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.interpretation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER interpretation_v1_append_only BEFORE UPDATE ON proxima_core.interpretation_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- memory
-- Generated presence family: integrity.rs::presence_trigger.
DROP TRIGGER IF EXISTS memory_declares_proxima_core_agent_derivation_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_agent_derivation_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.agent_derivation_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.agent_derivation_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxima_core_agent_note_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_agent_note_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.agent_note_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.agent_note_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxima_core_interpretation_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_interpretation_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.interpretation_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.interpretation_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxima_core_utterance_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_utterance_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.utterance_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.utterance_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxima_core_write_act_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_write_act_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.write_act_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.write_act_v1', 't');
DROP TRIGGER IF EXISTS memory_declares_proxima_core_goal_change_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_goal_change_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.goal_change_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.goal_change_v1', 't');
CREATE OR REPLACE TRIGGER memory_align_head BEFORE INSERT ON proxima_core.memory FOR EACH ROW EXECUTE FUNCTION proxima_core.memory_align_head();
CREATE OR REPLACE TRIGGER memory_append_only BEFORE UPDATE ON proxima_core.memory FOR EACH ROW EXECUTE FUNCTION proxima_core.memory_owner_or_append_only();
CREATE OR REPLACE TRIGGER memory_erased_pin_target BEFORE DELETE ON proxima_core.memory FOR EACH ROW EXECUTE FUNCTION proxima_core.memory_erase_witness();
CREATE OR REPLACE TRIGGER memory_forget_grounding BEFORE UPDATE OF forgotten_at ON proxima_core.memory FOR EACH ROW WHEN (((old.forgotten_at IS NULL) AND (new.forgotten_at IS NOT NULL))) EXECUTE FUNCTION proxima_core.cooled_forget_grounding();
CREATE OR REPLACE TRIGGER memory_pin_checks BEFORE INSERT ON proxima_core.memory FOR EACH ROW EXECUTE FUNCTION proxima_core.memory_pin_checks();
CREATE OR REPLACE TRIGGER memory_sidecar_stamp_declared BEFORE INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory FOR EACH ROW WHEN ((new.sidecar_tables <> '{}'::text[])) EXECUTE FUNCTION proxima_core.assert_sidecar_stamp_declared();


-- memory_head
CREATE OR REPLACE TRIGGER memory_head_t_only BEFORE UPDATE ON proxima_core.memory_head FOR EACH ROW EXECUTE FUNCTION proxima_core.memory_head_t_only();


-- owners
CREATE OR REPLACE TRIGGER owner_group_kind_guard BEFORE INSERT OR UPDATE ON proxima_core.owners FOR EACH ROW EXECUTE FUNCTION proxima_core.owner_group_kind_guard();
CREATE OR REPLACE TRIGGER owners_append_only BEFORE UPDATE ON proxima_core.owners FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();


-- projection
-- Generated projection trigger: projection.rs::projection_artifacts.
CREATE OR REPLACE TRIGGER projection_remember_lang
    BEFORE INSERT ON proxima_core.projection
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.remember_lexical_language();

-- publication_origin
CREATE OR REPLACE TRIGGER publication_origin_identity_immutable BEFORE UPDATE ON proxima_core.publication_origin FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_publication_origin_immutable();


-- publication_outbox
CREATE OR REPLACE TRIGGER publication_outbox_capture_immutable BEFORE UPDATE ON proxima_core.publication_outbox FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_publication_immutable();


-- utterance_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER utterance_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.utterance_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS utterance_v1_declared_by_memory_on_delete ON proxima_core.utterance_v1;
CREATE CONSTRAINT TRIGGER utterance_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.utterance_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.utterance_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER utterance_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.utterance_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
CREATE OR REPLACE TRIGGER utterance_v1_append_only BEFORE UPDATE ON proxima_core.utterance_v1 FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();

-- write_act_v1
-- Generated declaration family: integrity.rs::declaration_trigger.
CREATE OR REPLACE TRIGGER write_act_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.write_act_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
-- Generated delete guard family: integrity.rs::delete_guard_trigger.
DROP TRIGGER IF EXISTS write_act_v1_declared_by_memory_on_delete ON proxima_core.write_act_v1;
CREATE CONSTRAINT TRIGGER write_act_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.write_act_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.write_act_v1', 't');
-- Generated key repoint family: integrity.rs::key_repoint_trigger.
CREATE OR REPLACE TRIGGER write_act_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.write_act_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

-- store_identity
CREATE OR REPLACE TRIGGER store_identity_immutable
    BEFORE UPDATE OR DELETE ON proxima_core.store_identity
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_store_identity_immutable();

-- publication_counter
CREATE OR REPLACE TRIGGER publication_counter_forward_only
    BEFORE UPDATE ON proxima_core.publication_counter
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.publication_counter_forward_only();
$rename_history_triggers$::text WHERE false;
SELECT $rename_history_vector$-- Vector candidate functions of a store that keeps its vectors in pgvector.
-- Applied by the definitions pass only for that store kind, after functions.sql
-- (generated: vector_index.rs). A store with a host vector index never applies
-- this file and needs no `vector` extension.

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxima_core TO proxima_search;
GRANT SELECT ON proxima_core.embeddings, proxima_core.embedding_heads, proxima_core.memory TO proxima_search;
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embeddings;
CREATE POLICY proxima_search_candidates ON proxima_core.embeddings FOR SELECT TO proxima_search USING (true);
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embedding_heads;
CREATE POLICY proxima_search_candidates ON proxima_core.embedding_heads FOR SELECT TO proxima_search USING (true);
SET LOCAL ROLE proxima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxima_core.vector_candidates_d384(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 384 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxima_core.embeddings emb
             WHERE emb.dim = 384
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.vector(384) OPERATOR(public.<=>) $3::public.vector(384)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 384
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 384
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxima_core.vector_candidates_d384(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxima_core FROM proxima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxima_core TO proxima_search;
GRANT SELECT ON proxima_core.embeddings, proxima_core.embedding_heads, proxima_core.memory TO proxima_search;
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embeddings;
CREATE POLICY proxima_search_candidates ON proxima_core.embeddings FOR SELECT TO proxima_search USING (true);
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embedding_heads;
CREATE POLICY proxima_search_candidates ON proxima_core.embedding_heads FOR SELECT TO proxima_search USING (true);
SET LOCAL ROLE proxima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxima_core.vector_candidates_d768(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 768 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxima_core.embeddings emb
             WHERE emb.dim = 768
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.vector(768) OPERATOR(public.<=>) $3::public.vector(768)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 768
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 768
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxima_core.vector_candidates_d768(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxima_core FROM proxima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxima_core TO proxima_search;
GRANT SELECT ON proxima_core.embeddings, proxima_core.embedding_heads, proxima_core.memory TO proxima_search;
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embeddings;
CREATE POLICY proxima_search_candidates ON proxima_core.embeddings FOR SELECT TO proxima_search USING (true);
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embedding_heads;
CREATE POLICY proxima_search_candidates ON proxima_core.embedding_heads FOR SELECT TO proxima_search USING (true);
SET LOCAL ROLE proxima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxima_core.vector_candidates_d1024(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 1024 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxima_core.embeddings emb
             WHERE emb.dim = 1024
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.vector(1024) OPERATOR(public.<=>) $3::public.vector(1024)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 1024
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 1024
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxima_core.vector_candidates_d1024(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxima_core FROM proxima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxima_core TO proxima_search;
GRANT SELECT ON proxima_core.embeddings, proxima_core.embedding_heads, proxima_core.memory TO proxima_search;
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embeddings;
CREATE POLICY proxima_search_candidates ON proxima_core.embeddings FOR SELECT TO proxima_search USING (true);
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embedding_heads;
CREATE POLICY proxima_search_candidates ON proxima_core.embedding_heads FOR SELECT TO proxima_search USING (true);
SET LOCAL ROLE proxima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxima_core.vector_candidates_d1536(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 1536 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxima_core.embeddings emb
             WHERE emb.dim = 1536
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.vector(1536) OPERATOR(public.<=>) $3::public.vector(1536)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 1536
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 1536
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxima_core.vector_candidates_d1536(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxima_core FROM proxima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxima_core TO proxima_search;
GRANT SELECT ON proxima_core.embeddings, proxima_core.embedding_heads, proxima_core.memory TO proxima_search;
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embeddings;
CREATE POLICY proxima_search_candidates ON proxima_core.embeddings FOR SELECT TO proxima_search USING (true);
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embedding_heads;
CREATE POLICY proxima_search_candidates ON proxima_core.embedding_heads FOR SELECT TO proxima_search USING (true);
SET LOCAL ROLE proxima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxima_core.vector_candidates_d2048(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 2048 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxima_core.embeddings emb
             WHERE emb.dim = 2048
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.halfvec(2048) OPERATOR(public.<=>) $3::public.halfvec(2048)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 2048
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 2048
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxima_core.vector_candidates_d2048(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxima_core FROM proxima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxima_core TO proxima_search;
GRANT SELECT ON proxima_core.embeddings, proxima_core.embedding_heads, proxima_core.memory TO proxima_search;
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embeddings;
CREATE POLICY proxima_search_candidates ON proxima_core.embeddings FOR SELECT TO proxima_search USING (true);
DROP POLICY IF EXISTS proxima_search_candidates ON proxima_core.embedding_heads;
CREATE POLICY proxima_search_candidates ON proxima_core.embedding_heads FOR SELECT TO proxima_search USING (true);
SET LOCAL ROLE proxima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxima_core.vector_candidates_d3072(uuid[], text, public.vector, bigint)
                 RETURNS TABLE(memory_id uuid, similarity_score real)
                 LANGUAGE plpgsql STABLE SECURITY DEFINER
                 SET search_path = pg_catalog, pg_temp
                 AS $body$
DECLARE
    target bigint := LEAST(GREATEST($4, 0), 1000);
    chunk_window bigint := GREATEST(target, 1);
    candidate_ids uuid[] := '{}'::uuid[];
    scanned_ids uuid[];
    fetched bigint;
BEGIN
    PERFORM proxima_core.assert_search_scope($1);
    IF public.vector_dims($3) IS DISTINCT FROM 3072 THEN
        RAISE EXCEPTION 'query vector does not match index width'
            USING ERRCODE = '22023';
    END IF;
    IF target = 0 THEN
        RETURN;
    END IF;
    LOOP
        WITH scanned AS MATERIALIZED (
            SELECT emb.entity_id
              FROM proxima_core.embeddings emb
             WHERE emb.dim = 3072
               AND emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND EXISTS (
                   SELECT 1 FROM proxima_core.embedding_heads head
                    WHERE head.entity_id = emb.entity_id
                      AND head.model_id = emb.model_id
                      AND head.dim = emb.dim
                      AND head.embedding_version = emb.embedding_version
               )
               AND EXISTS (
                   SELECT 1 FROM proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
                    WHERE m.t = emb.entity_id
                      AND m.owner_id = ANY($1)
                      AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
               )
             ORDER BY emb.vec::public.halfvec(3072) OPERATOR(public.<=>) $3::public.halfvec(3072)
             LIMIT chunk_window
        )
        SELECT COALESCE(array_agg(DISTINCT s.entity_id), '{}'::uuid[]), count(*)
          INTO scanned_ids, fetched FROM scanned s;
        SELECT ARRAY(SELECT DISTINCT id FROM unnest(candidate_ids || scanned_ids) id)
          INTO candidate_ids;
        EXIT WHEN fetched < chunk_window
               OR cardinality(candidate_ids) >= target
               OR chunk_window >= 1000;
        chunk_window := LEAST(chunk_window * 2, 1000);
    END LOOP;
    IF cardinality(candidate_ids) < target THEN
        RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 3072
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
         ORDER BY emb.chunk_score DESC NULLS LAST, emb.entity_id DESC
         LIMIT 1000
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
        RETURN;
    END IF;
    RETURN QUERY
        WITH owned_embeddings AS MATERIALIZED (
            SELECT emb.entity_id, emb.model_id, emb.dim, emb.embedding_version,
                   NULLIF(1 - (emb.vec OPERATOR(public.<=>) $3), 'NaN'::float8) AS chunk_score
              FROM proxima_core.embeddings emb
             WHERE emb.owner_id = ANY($1)
               AND emb.model_id = $2
               AND emb.dim = 3072
               AND emb.entity_id = ANY(candidate_ids)
        ), eligible_chunks AS (
        SELECT emb.entity_id, emb.chunk_score
          FROM owned_embeddings emb
          JOIN proxima_core.embedding_heads head
            ON head.entity_id = emb.entity_id
           AND head.model_id = emb.model_id
           AND head.dim = emb.dim
           AND head.embedding_version = emb.embedding_version
          JOIN proxima_core.memory m /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ ON m.t = emb.entity_id
         WHERE m.owner_id = ANY($1)
           AND m.forgotten_at IS NULL /* FORGET-POLICY: live-only (semantic candidates under owner RLS) */
        )
        SELECT emb.entity_id,
               LEAST(1.0, GREATEST(0.0, MAX(emb.chunk_score)))::real AS score
          FROM eligible_chunks emb
         GROUP BY emb.entity_id
         ORDER BY score DESC, emb.entity_id DESC
         LIMIT target;
END;
$body$;$generated_function$;
REVOKE ALL ON FUNCTION proxima_core.vector_candidates_d3072(uuid[], text, public.vector, bigint) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxima_core FROM proxima_search;
END
$install_candidate$;
$rename_history_vector$::text WHERE false;

-- rename-finalization-begin

-- Catalog objects are renamed only after every immutable historical lane.
-- The temporary mapping and snapshots disappear when this session closes.
CREATE OR REPLACE FUNCTION pg_temp.rename_spiice_word(value text) RETURNS text
LANGUAGE plpgsql IMMUTABLE STRICT AS $mapping$
DECLARE rest text := value; result text := ''; at integer; word text; replacement text; i integer;
BEGIN
    LOOP
        at := regexp_instr(rest, 'flavou?r|(?<!ap)proxima', 1, 1, 0, 'i');
        EXIT WHEN at = 0;
        word := (regexp_match(substr(rest, at), '^(flavou?r|proxima)', 'i'))[1];
        replacement := CASE WHEN lower(word) LIKE 'fla%' THEN 'spiice' ELSE 'proxiima' END;
        IF word = upper(word) THEN replacement := upper(replacement);
        ELSIF word = initcap(lower(word)) THEN replacement := initcap(replacement);
        ELSIF word <> lower(word) THEN
            FOR i IN 1..length(replacement) LOOP
                IF substr(word, least(i, length(word)), 1) = upper(substr(word, least(i, length(word)), 1)) THEN
                    replacement := overlay(replacement placing upper(substr(replacement,i,1)) from i for 1);
                END IF;
            END LOOP;
        END IF;
        IF lower(word)='proxima' AND (
            (word=upper(word) AND (substr(rest,at+length(word),1)='L' OR substr(rest,at+length(word),2)='TE'))
            OR (word<>upper(word) AND (substr(rest,at+length(word),1)='l' OR substr(rest,at+length(word),2)='te'))
        ) THEN replacement:=word; END IF;
        result := result || substr(rest, 1, at-1) || replacement;
        rest := substr(rest, at + length(word));
    END LOOP;
    RETURN regexp_replace(result || rest, '(Aquilo-Solution-S/)proxiima', '\1proxiima', 'gi');
END $mapping$;

CREATE TEMP TABLE rename_spiice_definitions(kind text, schema_name text, table_name text, object_name text, statement text, owner_name text) ON COMMIT DROP;
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement,owner_name)
SELECT 'function', n.nspname, '', p.proname, pg_temp.rename_spiice_word(pg_get_functiondef(p.oid)), pg_get_userbyid(p.proowner)
FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
WHERE n.nspname !~ '^(pg_|information_schema$)' AND p.prokind IN ('f','p')
AND pg_get_functiondef(p.oid) <> pg_temp.rename_spiice_word(pg_get_functiondef(p.oid));
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'trigger', n.nspname, c.relname, t.tgname, pg_temp.rename_spiice_word(pg_get_triggerdef(t.oid, true))
FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)' AND NOT t.tgisinternal
AND pg_get_triggerdef(t.oid, true) <> pg_temp.rename_spiice_word(pg_get_triggerdef(t.oid, true));
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'view', n.nspname, c.relname, c.relname,
       pg_temp.rename_spiice_word(format('CREATE OR REPLACE VIEW %I.%I AS %s', n.nspname, c.relname, pg_get_viewdef(c.oid, true)))
FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)' AND c.relkind='v'
AND format('%I.%I %s',n.nspname,c.relname,pg_get_viewdef(c.oid,true)) <>
    pg_temp.rename_spiice_word(format('%I.%I %s',n.nspname,c.relname,pg_get_viewdef(c.oid,true)));
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'policy', n.nspname, c.relname, p.polname,
       pg_temp.rename_spiice_word(format('CREATE POLICY %I ON %I.%I AS %s FOR %s TO %s%s%s',
         p.polname,n.nspname,c.relname,CASE WHEN p.polpermissive THEN 'PERMISSIVE' ELSE 'RESTRICTIVE' END,
         CASE p.polcmd WHEN '*' THEN 'ALL' WHEN 'r' THEN 'SELECT' WHEN 'a' THEN 'INSERT' WHEN 'w' THEN 'UPDATE' WHEN 'd' THEN 'DELETE' END,
         (SELECT string_agg(CASE WHEN role_oid=0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(role_oid)) END, ', ' ORDER BY role_oid) FROM unnest(p.polroles) role_oid),
         CASE WHEN p.polqual IS NULL THEN '' ELSE ' USING ('||pg_get_expr(p.polqual,p.polrelid)||')' END,
         CASE WHEN p.polwithcheck IS NULL THEN '' ELSE ' WITH CHECK ('||pg_get_expr(p.polwithcheck,p.polrelid)||')' END))
FROM pg_policy p JOIN pg_class c ON c.oid=p.polrelid JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)';

INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'constraint', n.nspname,c.relname,k.conname,
       pg_temp.rename_spiice_word(format('ALTER TABLE %I.%I ADD CONSTRAINT %I %s',n.nspname,c.relname,k.conname,pg_get_constraintdef(k.oid)))
FROM pg_constraint k JOIN pg_class c ON c.oid=k.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)' AND k.contype='c'
AND pg_get_constraintdef(k.oid)<>pg_temp.rename_spiice_word(pg_get_constraintdef(k.oid));
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'default',n.nspname,c.relname,a.attname,
       pg_temp.rename_spiice_word(CASE WHEN a.attgenerated<>'' THEN format('ALTER TABLE %I.%I ALTER COLUMN %I SET EXPRESSION AS (%s)',n.nspname,c.relname,a.attname,pg_get_expr(d.adbin,d.adrelid)) ELSE format('ALTER TABLE %I.%I ALTER COLUMN %I SET DEFAULT %s',n.nspname,c.relname,a.attname,pg_get_expr(d.adbin,d.adrelid)) END)
FROM pg_attrdef d JOIN pg_attribute a ON a.attrelid=d.adrelid AND a.attnum=d.adnum JOIN pg_class c ON c.oid=d.adrelid JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)'
AND pg_get_expr(d.adbin,d.adrelid)<>pg_temp.rename_spiice_word(pg_get_expr(d.adbin,d.adrelid));
INSERT INTO rename_spiice_definitions(kind,schema_name,table_name,object_name,statement)
SELECT 'index',n.nspname,c.relname,i.relname,pg_temp.rename_spiice_word(pg_get_indexdef(x.indexrelid))
FROM pg_index x JOIN pg_class i ON i.oid=x.indexrelid JOIN pg_class c ON c.oid=x.indrelid JOIN pg_namespace n ON n.oid=c.relnamespace
WHERE n.nspname !~ '^(pg_|information_schema$)' AND NOT EXISTS(SELECT 1 FROM pg_constraint k WHERE k.conindid=x.indexrelid)
AND coalesce(pg_get_expr(x.indexprs,x.indrelid),'')||coalesce(pg_get_expr(x.indpred,x.indrelid),'') <>
    pg_temp.rename_spiice_word(coalesce(pg_get_expr(x.indexprs,x.indrelid),'')||coalesce(pg_get_expr(x.indpred,x.indrelid),''));

DO $rename$
DECLARE object record; statement record; new_name text; temporary_create boolean;
BEGIN
    -- Drop policies/triggers before renaming columns; their saved expressions
    -- include literal sidecar names which Postgres cannot update by object ID.
    FOR object IN SELECT * FROM rename_spiice_definitions WHERE kind IN ('policy','trigger','constraint','index') ORDER BY kind,schema_name,table_name,object_name LOOP
        IF object.kind='constraint' THEN
            EXECUTE format('ALTER TABLE %I.%I DROP CONSTRAINT %I',object.schema_name,object.table_name,object.object_name);
        ELSIF object.kind='index' THEN
            EXECUTE format('DROP INDEX %I.%I',object.schema_name,object.object_name);
        ELSE EXECUTE format('DROP %s %I ON %I.%I', upper(object.kind),object.object_name,object.schema_name,object.table_name);
        END IF;
    END LOOP;
    FOR object IN SELECT n.nspname,c.relname,a.attname FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND c.relkind IN ('r','p','v','m','f','i','I') AND a.attnum>0 AND NOT a.attisdropped
        AND a.attname<>pg_temp.rename_spiice_word(a.attname) ORDER BY n.nspname,c.relname,a.attnum LOOP
        EXECUTE format('ALTER TABLE %I.%I RENAME COLUMN %I TO %I',object.nspname,object.relname,object.attname,pg_temp.rename_spiice_word(object.attname));
    END LOOP;
    FOR object IN SELECT n.nspname,c.relname,k.conname FROM pg_constraint k JOIN pg_class c ON c.oid=k.conrelid JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND k.conname<>pg_temp.rename_spiice_word(k.conname) ORDER BY n.nspname,c.relname,k.conname LOOP
        EXECUTE format('ALTER TABLE %I.%I RENAME CONSTRAINT %I TO %I',object.nspname,object.relname,object.conname,pg_temp.rename_spiice_word(object.conname));
    END LOOP;
    FOR object IN SELECT n.nspname,t.typname,e.enumlabel FROM pg_enum e JOIN pg_type t ON t.oid=e.enumtypid JOIN pg_namespace n ON n.oid=t.typnamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND e.enumlabel<>pg_temp.rename_spiice_word(e.enumlabel) ORDER BY n.nspname,t.typname,e.enumsortorder LOOP
        EXECUTE format('ALTER TYPE %I.%I RENAME VALUE %L TO %L',object.nspname,object.typname,object.enumlabel,pg_temp.rename_spiice_word(object.enumlabel));
    END LOOP;
    FOR object IN SELECT n.nspname,p.proname,pg_get_function_identity_arguments(p.oid) args FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND p.prokind IN ('f','p') AND p.proname<>pg_temp.rename_spiice_word(p.proname) ORDER BY n.nspname,p.proname,p.oid LOOP
        EXECUTE format('ALTER ROUTINE %I.%I(%s) RENAME TO %I',object.nspname,object.proname,object.args,pg_temp.rename_spiice_word(object.proname));
    END LOOP;
    FOR object IN SELECT n.nspname,t.typname FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND t.typrelid=0 AND t.typelem=0
        AND t.typname<>pg_temp.rename_spiice_word(t.typname) ORDER BY n.nspname,t.typname LOOP
        EXECUTE format('ALTER TYPE %I.%I RENAME TO %I',object.nspname,object.typname,pg_temp.rename_spiice_word(object.typname));
    END LOOP;
    -- Constraint-backed indexes were renamed with their constraint above.
    FOR object IN SELECT n.nspname,c.relname,c.relkind FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname !~ '^(pg_|information_schema$)' AND c.relkind IN ('r','p','v','m','S','i','I','f')
        AND c.relname<>pg_temp.rename_spiice_word(c.relname) ORDER BY n.nspname,c.relname LOOP
        EXECUTE format('ALTER %s %I.%I RENAME TO %I',
          CASE object.relkind WHEN 'S' THEN 'SEQUENCE' WHEN 'i' THEN 'INDEX' WHEN 'I' THEN 'INDEX' WHEN 'v' THEN 'VIEW' WHEN 'm' THEN 'MATERIALIZED VIEW' WHEN 'f' THEN 'FOREIGN TABLE' ELSE 'TABLE' END,
          object.nspname,object.relname,pg_temp.rename_spiice_word(object.relname));
    END LOOP;
    FOR object IN SELECT nspname FROM pg_namespace WHERE nspname !~ '^pg_' AND nspname<>pg_temp.rename_spiice_word(nspname) ORDER BY nspname LOOP
        EXECUTE format('ALTER SCHEMA %I RENAME TO %I',object.nspname,pg_temp.rename_spiice_word(object.nspname));
    END LOOP;
    -- Bodies are restored from catalog definitions, with names rewritten by
    -- the same rule as the operator's source transformation.
    PERFORM set_config('check_function_bodies','off',true);
    FOR statement IN SELECT * FROM rename_spiice_definitions ORDER BY CASE kind WHEN 'function' THEN 0 WHEN 'view' THEN 1 WHEN 'constraint' THEN 2 WHEN 'index' THEN 3 WHEN 'default' THEN 4 WHEN 'trigger' THEN 5 ELSE 6 END,schema_name,table_name,object_name LOOP
        IF statement.kind='function' THEN
            new_name:=pg_temp.rename_spiice_word(statement.schema_name);
            temporary_create:=NOT has_schema_privilege(statement.owner_name,new_name,'CREATE');
            IF temporary_create THEN EXECUTE format('GRANT CREATE ON SCHEMA %I TO %I',new_name,statement.owner_name); END IF;
            EXECUTE format('SET LOCAL ROLE %I',statement.owner_name);
            EXECUTE statement.statement;
            RESET ROLE;
            IF temporary_create THEN EXECUTE format('REVOKE CREATE ON SCHEMA %I FROM %I',new_name,statement.owner_name); END IF;
        ELSE EXECUTE statement.statement;
        END IF;
    END LOOP;
    -- Registry data contains SQL identifiers, rather than user prose.
    IF to_regclass('proxiima_core.spiice_surface') IS NOT NULL THEN
        UPDATE proxiima_core.spiice_surface SET table_name=pg_temp.rename_spiice_word(table_name),spiice_id=pg_temp.rename_spiice_word(spiice_id);
    END IF;
    IF to_regclass('proxiima_core.memory') IS NOT NULL THEN
        UPDATE proxiima_core.memory SET sidecar_tables=ARRAY(SELECT pg_temp.rename_spiice_word(x) FROM unnest(sidecar_tables) x)
        WHERE EXISTS(SELECT 1 FROM unnest(sidecar_tables) x WHERE x<>pg_temp.rename_spiice_word(x));
    END IF;

    FOR object IN
        SELECT d.description, identified.type AS kind, identified.identity
          FROM pg_description d
          CROSS JOIN LATERAL pg_identify_object(d.classoid,d.objoid,d.objsubid) identified
         WHERE d.objoid>=16384 AND coalesce(identified.schema,'') !~ '^pg_'
           AND d.description<>pg_temp.rename_spiice_word(d.description)
         ORDER BY identified.type,identified.identity
    LOOP
        new_name:=CASE object.kind
            WHEN 'table column' THEN 'COLUMN' WHEN 'view column' THEN 'COLUMN'
            WHEN 'materialized view column' THEN 'COLUMN' WHEN 'foreign table column' THEN 'COLUMN'
            WHEN 'table constraint' THEN 'CONSTRAINT'
            WHEN 'table' THEN 'TABLE' WHEN 'foreign table' THEN 'FOREIGN TABLE'
            WHEN 'view' THEN 'VIEW' WHEN 'materialized view' THEN 'MATERIALIZED VIEW'
            WHEN 'sequence' THEN 'SEQUENCE' WHEN 'index' THEN 'INDEX'
            WHEN 'function' THEN 'FUNCTION' WHEN 'procedure' THEN 'PROCEDURE'
            WHEN 'type' THEN 'TYPE' WHEN 'domain' THEN 'DOMAIN' WHEN 'schema' THEN 'SCHEMA'
            WHEN 'trigger' THEN 'TRIGGER' WHEN 'policy' THEN 'POLICY'
            ELSE NULL END;
        IF new_name IS NULL THEN RAISE EXCEPTION 'unsupported catalog comment kind %',object.kind; END IF;
        EXECUTE format('COMMENT ON %s %s IS %L',new_name,object.identity,pg_temp.rename_spiice_word(object.description));
    END LOOP;
END $rename$;
DROP TABLE rename_spiice_definitions;
DROP FUNCTION pg_temp.rename_spiice_word(text);
