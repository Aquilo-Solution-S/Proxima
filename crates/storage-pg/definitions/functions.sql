-- Functions and aggregate definitions owned by the proxima_core catalog.
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
                  HINT = 'a stamp with no row cools into a cold object whose sidecar dump '
                         || 'cannot equal its own stamp, so the Memory forgets and can never '
                         || 'be hydrated; write through Engine::unit_of_work, which stamps '
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
                  HINT = 'a stamp whose row was deleted cools into a cold object whose '
                         || 'sidecar dump cannot equal its own stamp, so the Memory forgets '
                         || 'and can never be hydrated, and the row is gone so nothing can '
                         || 'repair it; delete the memory row in the same transaction, which '
                         || 'is what forget and owner erase do';
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

-- Cooling functions

CREATE OR REPLACE FUNCTION proxima_core.cooled_append_only()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    IF NEW.t IS DISTINCT FROM OLD.t
       OR NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW."by" IS DISTINCT FROM OLD."by"
       OR NEW.via IS DISTINCT FROM OLD.via
       OR NEW.object_key IS DISTINCT FROM OLD.object_key
       OR NEW.source_id IS DISTINCT FROM OLD.source_id
       OR NEW.ingest_key IS DISTINCT FROM OLD.ingest_key
       OR NEW.origins IS DISTINCT FROM OLD.origins
       OR NEW.refs IS DISTINCT FROM OLD.refs
       OR NEW.goal_refs IS DISTINCT FROM OLD.goal_refs
       OR NEW.cooled_at IS DISTINCT FROM OLD.cooled_at
       OR NEW.cold_digest IS DISTINCT FROM OLD.cold_digest
    THEN
        RAISE EXCEPTION
            'cooled is frozen except owner_id, blob_id and content_id remaps'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$function$;

-- FORGET-POLICY: includes-forgotten (cooled lifecycle, removed in S16) applies to every read in this function
CREATE OR REPLACE FUNCTION proxima_core.cooled_erase_witness()
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
    -- Cooled -> Memory is hydration, not hard erase.
    IF NOT EXISTS (SELECT 1 FROM proxima_core.memory WHERE t = OLD.t) THEN
        PERFORM proxima_core.record_erased_pin_target(
            OLD.t, OLD.kind::text::proxima_core.pin_target_kind
        );
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN OLD;
END;
$function$;
REVOKE ALL ON FUNCTION proxima_core.cooled_erase_witness() FROM PUBLIC;

-- FORGET-POLICY: includes-forgotten (cooled lifecycle, removed in S16) applies to every read in this function
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
$function$;
REVOKE ALL ON FUNCTION proxima_core.cooled_forget_grounding() FROM PUBLIC;

-- FORGET-POLICY: includes-forgotten (cooled lifecycle, removed in S16) applies to every read in this function
CREATE OR REPLACE FUNCTION proxima_core.cooled_identity_seal()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
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
           AND m."by" = NEW."by"
           AND m.via IS NOT DISTINCT FROM NEW.via
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
$function$;
REVOKE ALL ON FUNCTION proxima_core.cooled_identity_seal() FROM PUBLIC;

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
       OR EXISTS (SELECT 1 FROM proxima_core.cooled WHERE t = NEW.t)
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
BEGIN
    IF target_schema IS NULL OR target_schema IN ('proxima_core', 'public', 'information_schema')
       OR target_schema LIKE 'pg\_%' THEN
        RAISE EXCEPTION 'install_owner_rls: % is not a flavor schema', COALESCE(target_schema, 'NULL');
    END IF;
    IF to_regnamespace(quote_ident(target_schema)) IS NULL THEN
        RAISE EXCEPTION 'install_owner_rls: schema % does not exist', target_schema;
    END IF;
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
        IF relation.relname <> ALL(classified) THEN
            RAISE EXCEPTION 'owner RLS classification missing for %.%', relation.schema_name, relation.relname;
        END IF;
        IF relation.has_owner_id AND relation.relname <> ALL(COALESCE(owner_id_tables, '{}')
                || COALESCE(memory_owner_tables, '{}')) THEN
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
COMMENT ON FUNCTION proxima_core.install_owner_rls(target_schema text, owner_id_tables text[], fk_parent_tables text[], ownerless_tables text[], memory_owner_tables text[]) IS 'Owner-RLS policies for one flavor schema from its table classification; refuses an unclassified table (docs/09 §Owner RLS).';

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
COMMENT ON FUNCTION proxima_core.install_owner_rls_table(target_schema text, target_table text, class proxima_core.owner_rls_class, owner_column text) IS 'Owner-RLS policies for one table of a flavor or host schema; the per-table form of install_owner_rls (docs/09 §Owner RLS).';

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
    historical_restore boolean;
    pin_targets uuid[] := ARRAY[NEW.t] || NEW.origins || NEW.refs || NEW.goal_refs;
    pin_lock_mode text := current_setting('proxima_core.pin_lock_mode', true);
    pin_lock_count integer;
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    -- Until decided, the setting counts the target locks this transaction
    -- asked for here. Past 256, one pg_locks read decides the rest of the
    -- transaction; it scans the whole lock table, so a small write never pays
    -- it. 'coarse': memory, cooled and goal are all held in SHARE ROW
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
            SELECT CASE WHEN count(DISTINCT l.relation) = 3 THEN 'coarse' ELSE 'fine' END
              INTO pin_lock_mode
              FROM pg_catalog.pg_locks l
             WHERE l.locktype = 'relation'
               AND l.pid = pg_catalog.pg_backend_pid()
               AND l.granted
               AND l.mode IN ('ShareRowExclusiveLock', 'ExclusiveLock', 'AccessExclusiveLock')
               AND l.relation IN ('proxima_core.memory'::regclass,
                                  'proxima_core.cooled'::regclass,
                                  'proxima_core.goal'::regclass);
        END IF;
        PERFORM set_config('proxima_core.pin_lock_mode', pin_lock_mode, true);
    END IF;
    -- Take the table locks again instead of trusting the setting: held, this
    -- is a local no-op; a setting bound without them acquires them before any
    -- check below reads.
    IF pin_lock_mode = 'coarse' THEN
        LOCK TABLE proxima_core.cooled, proxima_core.memory, proxima_core.goal
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

    SELECT EXISTS (
        SELECT 1
          FROM proxima_core.cooled c
         WHERE c.t = NEW.t
           AND c.handle = NEW.handle
           AND c.owner_id = NEW.owner_id
           AND c.kind = NEW.kind
           AND c."by" = NEW."by"
           AND c.via IS NOT DISTINCT FROM NEW.via
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
               AND c."by" = NEW."by"
               AND c.via IS NOT DISTINCT FROM NEW.via
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
    rel_owner name;
    read_expr text;
    write_expr text;
BEGIN
    SELECT pg_get_userbyid(c.relowner) INTO rel_owner
      FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
     WHERE n.nspname = target_schema AND c.relname = target_table AND c.relkind IN ('r', 'p');

    SELECT predicates.read_expr, predicates.write_expr
      INTO read_expr, write_expr
      FROM proxima_core.owner_rls_class_predicates(
          caller, class_label, target_schema, target_table, class, owner_column
      ) AS predicates;

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
REVOKE ALL ON FUNCTION proxima_core.owner_rls_apply_class(caller text, class_label text, target_schema text, target_table text, class proxima_core.owner_rls_class, owner_column text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxima_core.owner_rls_census_table(target_schema text, target_table text)
 RETURNS void
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    relation record;
BEGIN
    SELECT c.oid, n.nspname AS schema_name, c.relname, c.relowner,
           c.relrowsecurity, c.relforcerowsecurity
      INTO relation
      FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
     WHERE n.nspname = target_schema AND c.relname = target_table AND c.relkind IN ('r', 'p');
    IF NOT relation.relrowsecurity OR NOT relation.relforcerowsecurity THEN
        RAISE EXCEPTION 'table %.% lacks ENABLE/FORCE RLS', relation.schema_name, relation.relname;
    END IF;
    IF (SELECT count(*) FROM pg_policy WHERE polrelid = relation.oid) <> 3 THEN
        RAISE EXCEPTION 'table %.% does not have exactly three RLS policies', relation.schema_name, relation.relname;
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
