-- Functions and aggregate definitions owned by the proxiima_core catalog.
-- This file is the source for current core function definitions.
-- Definitions are replaceable; runtime and platform grants come from defaults.

-- Retire the six fixed-width vector candidates from the released pre-heads-only
-- contract in the common definitions pass, for both vector backends. Historical
-- replay invalidates its definitions record before this bundle is reinstalled.
-- A HostIndex database without pgvector has no overloads to retire.
DO $retire_legacy_vector_candidates$
DECLARE
    original_role name := current_user;
    width integer;
BEGIN
    IF to_regtype('public.vector') IS NOT NULL THEN
        EXECUTE 'SET LOCAL ROLE proxiima_search';
        FOREACH width IN ARRAY ARRAY[384, 768, 1024, 1536, 2048, 3072] LOOP
            EXECUTE format(
                'DROP FUNCTION IF EXISTS proxiima_core.vector_candidates_d%s(uuid[], text, public.vector, bigint)',
                width
            );
        END LOOP;
        EXECUTE format('SET LOCAL ROLE %I', original_role);
    END IF;
END
$retire_legacy_vector_candidates$;

-- Assertion functions

-- Schema evolution fence.  The schema id is already the complete SchemaRef
-- identity (including its version where applicable), so no second version
-- column or registry is involved.  The namespace keeps these locks separate
-- from memory-series and natural-key locks while remaining reproducible by
-- the migration executor.
CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_lock_key(schema_id text)
 RETURNS bigint
 LANGUAGE sql
 IMMUTABLE
 STRICT
 SET search_path TO 'pg_catalog'
AS $function$
    SELECT hashtextextended('proxiima-schema-evolution:' || schema_id, 0)
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.lock_schema_evolution_shared(schema_id text)
 RETURNS void
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
BEGIN
    IF schema_id IS NULL OR btrim(schema_id) = '' THEN
        RAISE EXCEPTION 'schema evolution lock requires a nonblank schema id'
            USING ERRCODE = '22023';
    END IF;
    PERFORM pg_advisory_xact_lock_shared(
        proxiima_core.schema_evolution_lock_key(schema_id)
    );
END
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.lock_schema_evolution_exclusive(schema_id text)
 RETURNS void
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    relation_owner name;
BEGIN
    IF schema_id IS NULL OR btrim(schema_id) = '' THEN
        RAISE EXCEPTION 'schema evolution lock requires a nonblank schema id'
            USING ERRCODE = '22023';
    END IF;
    IF current_setting('app.proxiima_scope', true) IS DISTINCT FROM 'platform'
       OR EXISTS (
           SELECT 1
             FROM pg_roles
            WHERE rolname = current_user
              AND (rolsuper OR rolbypassrls)
       )
    THEN
        RAISE EXCEPTION 'schema evolution exclusive lock requires the validated platform migration lane'
            USING ERRCODE = '42501';
    END IF;
    SELECT pg_get_userbyid(relowner)
      INTO relation_owner
      FROM pg_class
     WHERE oid = 'proxiima_core.schema_evolution'::regclass;
    IF relation_owner IS NULL OR NOT pg_has_role(current_user, relation_owner, 'USAGE') THEN
        RAISE EXCEPTION 'schema evolution exclusive lock requires schema metadata ownership'
            USING ERRCODE = '42501';
    END IF;
    PERFORM pg_advisory_xact_lock(
        proxiima_core.schema_evolution_lock_key(schema_id)
    );
END
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.assert_schema_evolution_source_writable()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    incoming_schema_id text;
    incoming_kind text;
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    incoming_schema_id := NEW.schema_id;
    IF TG_TABLE_NAME = 'memory' THEN
        incoming_kind := NEW.kind::text;
    ELSIF TG_TABLE_NAME = 'goal_head' THEN
        incoming_kind := 'goal';
    ELSIF TG_TABLE_NAME = 'blob' THEN
        incoming_kind := 'cited_object';
    ELSE
        RAISE EXCEPTION 'unsupported schema evolution fence relation' USING ERRCODE = '55000';
    END IF;
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    PERFORM proxiima_core.lock_schema_evolution_shared(incoming_schema_id);
    IF EXISTS (
        SELECT 1
          FROM proxiima_core.schema_evolution evolution
         WHERE evolution.source_kind = incoming_kind
           AND evolution.source_schema_id = incoming_schema_id
    ) THEN
        RAISE EXCEPTION
            'schema % (%) has completed evolution; run the declared migration before writing it',
            incoming_schema_id, incoming_kind
            USING ERRCODE = '55000',
                  HINT = 'write one of the declared target schemas with the migration-aware binary';
    END IF;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.lock_schema_evolution_exclusive(text) FROM PUBLIC;
REVOKE ALL ON FUNCTION proxiima_core.assert_schema_evolution_source_writable() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_permits(
    requested_kind text, source_id uuid, target_id uuid
) RETURNS boolean
 LANGUAGE sql
 STABLE
 SET search_path TO 'pg_catalog', 'proxiima_core'
AS $function$
    SELECT current_setting('app.proxiima_scope', true) = 'platform'
       AND NOT EXISTS (
           SELECT 1 FROM pg_roles
            WHERE rolname = current_user AND (rolsuper OR rolbypassrls)
       )
       AND pg_has_role(current_user,
           (SELECT relowner FROM pg_class
             WHERE oid = 'proxiima_core.schema_evolution_authority'::regclass), 'USAGE')
       AND EXISTS (
           SELECT 1
             FROM proxiima_core.schema_evolution_authority authority
            WHERE authority.xact = pg_current_xact_id()
              AND authority.record_kind = requested_kind
              AND authority.source_id = schema_evolution_permits.source_id
              AND authority.target_id = schema_evolution_permits.target_id
       )
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_head_permits(
    requested_kind text, source_id uuid, target_id uuid,
    source_handle uuid, target_handle uuid
) RETURNS boolean
 LANGUAGE sql STABLE
 SET search_path TO 'pg_catalog', 'proxiima_core'
AS $function$
    SELECT proxiima_core.schema_evolution_permits(requested_kind, source_id, target_id)
       AND EXISTS (
           SELECT 1 FROM proxiima_core.schema_evolution_authority authority
            WHERE authority.xact = pg_current_xact_id()
              AND authority.record_kind = requested_kind
              AND authority.source_id = schema_evolution_head_permits.source_id
              AND authority.target_id = schema_evolution_head_permits.target_id
              AND authority.source_handle IS NOT DISTINCT FROM schema_evolution_head_permits.source_handle
              AND authority.target_handle IS NOT DISTINCT FROM schema_evolution_head_permits.target_handle
       )
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_mapping_matches(
    requested_kind text, source_id uuid, target_id uuid,
    source_schema text, target_schema text, record_owner uuid
) RETURNS boolean
 LANGUAGE sql STABLE
 SET search_path TO 'pg_catalog', 'proxiima_core'
AS $function$
    SELECT proxiima_core.schema_evolution_permits(requested_kind, source_id, target_id)
       AND EXISTS (
           SELECT 1 FROM proxiima_core.schema_evolution_authority authority
            WHERE authority.xact = pg_current_xact_id()
              AND authority.record_kind = requested_kind
              AND authority.source_id = schema_evolution_mapping_matches.source_id
              AND authority.target_id = schema_evolution_mapping_matches.target_id
              AND authority.source_schema = schema_evolution_mapping_matches.source_schema
              AND authority.target_schema = schema_evolution_mapping_matches.target_schema
              AND authority.record_owner = schema_evolution_mapping_matches.record_owner
       )
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_target_matches(
    requested_kind text, source_id uuid, target_id uuid,
    target_schema text, record_owner uuid
) RETURNS boolean
 LANGUAGE sql STABLE
 SET search_path TO 'pg_catalog', 'proxiima_core'
AS $function$
    SELECT proxiima_core.schema_evolution_permits(requested_kind, source_id, target_id)
       AND EXISTS (
           SELECT 1 FROM proxiima_core.schema_evolution_authority authority
            WHERE authority.xact = pg_current_xact_id()
              AND authority.record_kind = requested_kind
              AND authority.source_id = schema_evolution_target_matches.source_id
              AND authority.target_id = schema_evolution_target_matches.target_id
              AND authority.target_schema = schema_evolution_target_matches.target_schema
              AND authority.record_owner = schema_evolution_target_matches.record_owner
       )
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_intermediate_memory_image_permits(
    memory_t uuid, image_schema text, final_schema text, memory_handle uuid,
    memory_kind proxiima_core.memory_kind, memory_owner uuid
) RETURNS boolean
 LANGUAGE sql STABLE
 SET search_path TO 'pg_catalog', 'proxiima_core'
AS $function$
    SELECT image_schema IS DISTINCT FROM final_schema
       AND proxiima_core.schema_evolution_mapping_matches(
           'memory', memory_t, memory_t, image_schema, final_schema, memory_owner
       )
       AND EXISTS (
           SELECT 1
             FROM proxiima_core.memory_head head
            WHERE head.handle = memory_handle
              AND head.kind = memory_kind
              AND head.owner_id = memory_owner
              AND head.schema_id = final_schema
       )
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_intermediate_memory_schema_permits(
    memory_t uuid, image_schema text
) RETURNS boolean
 LANGUAGE sql STABLE
 SET search_path TO 'pg_catalog', 'proxiima_core'
AS $function$
    SELECT proxiima_core.schema_evolution_permits('memory', memory_t, memory_t)
       AND EXISTS (
           SELECT 1
             FROM proxiima_core.schema_evolution_authority authority
            WHERE authority.xact = pg_current_xact_id()
              AND authority.record_kind = 'memory'
              AND authority.source_id = memory_t
              AND authority.target_id = memory_t
              AND authority.source_schema = image_schema
              AND authority.target_schema IS DISTINCT FROM image_schema
              AND authority.source_handle IS NOT DISTINCT FROM authority.target_handle
       )
$function$;

-- A historical witness may survive only when the exact source row being
-- migrated already carried that pin.  This is deliberately narrower than a
-- migration-wide waiver: an ordinary insert has no authority row, and a
-- target cannot invent a reference to an erased identity.
CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_existing_pin_permits(
    requested_kind text, target_id uuid, pin uuid
) RETURNS boolean
 LANGUAGE sql STABLE SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
    SELECT EXISTS (
        SELECT 1
          FROM proxiima_core.schema_evolution_authority a
         WHERE requested_kind = 'memory'
           AND a.xact = pg_current_xact_id()
           AND a.record_kind = 'memory'
           AND a.target_id = schema_evolution_existing_pin_permits.target_id
           AND pin = ANY (a.source_pins)
        UNION ALL
        SELECT 1
          FROM proxiima_core.schema_evolution_authority a
         WHERE requested_kind = 'goal'
           AND a.xact = pg_current_xact_id()
           AND a.record_kind = 'goal'
           AND a.target_id = schema_evolution_existing_pin_permits.target_id
           AND pin = ANY (a.source_pins)
    )
$function$;
REVOKE ALL ON FUNCTION proxiima_core.schema_evolution_existing_pin_permits(text, uuid, uuid) FROM PUBLIC;

-- FORGET-POLICY: includes-forgotten (snapshot original pins before representation migration) applies to every read in this function
CREATE OR REPLACE FUNCTION proxiima_core.assert_schema_evolution_authority_writer()
 RETURNS trigger LANGUAGE plpgsql
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
BEGIN
    IF current_setting('app.proxiima_scope', true) IS DISTINCT FROM 'platform'
       OR EXISTS (SELECT 1 FROM pg_roles WHERE rolname = current_user AND (rolsuper OR rolbypassrls))
       OR NOT pg_has_role(current_user,
           (SELECT relowner FROM pg_class WHERE oid = 'proxiima_core.schema_evolution_authority'::regclass), 'USAGE')
    THEN
        RAISE EXCEPTION 'schema evolution authority is writable only by the validated migration lane'
            USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    IF TG_OP = 'UPDATE' THEN
        IF NEW.record_kind IS DISTINCT FROM OLD.record_kind
           OR NEW.source_id IS DISTINCT FROM OLD.source_id
           OR NEW.target_id IS DISTINCT FROM OLD.target_id
           OR NEW.source_schema IS DISTINCT FROM OLD.source_schema
           OR NEW.target_schema IS DISTINCT FROM OLD.target_schema
           OR NEW.source_handle IS DISTINCT FROM OLD.source_handle
           OR NEW.target_handle IS DISTINCT FROM OLD.target_handle
           OR NEW.record_owner IS DISTINCT FROM OLD.record_owner
           OR NEW.primary_target IS DISTINCT FROM OLD.primary_target
           OR NEW.source_pins IS DISTINCT FROM OLD.source_pins THEN
            RAISE EXCEPTION 'schema evolution authority is immutable' USING ERRCODE = '25006';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.record_kind = 'memory' THEN
        SELECT source.origins || source.refs || source.goal_refs || source.guided_by
               || CASE WHEN source.forked_from IS NULL THEN '{}'::uuid[] ELSE ARRAY[source.forked_from] END
          INTO NEW.source_pins
          FROM proxiima_core.memory source
         WHERE source.t = NEW.source_id
           AND source.schema_id = NEW.source_schema
           AND source.owner_id = NEW.record_owner;
    ELSIF NEW.record_kind = 'goal' THEN
        SELECT array_remove(ARRAY[source.close_fact_t, source.assignment_t, source.write_act_t], NULL)
               || source.dependency_t || source.evidence_t || source.motivated_by
          INTO NEW.source_pins
          FROM proxiima_core.goal source
         WHERE source.t = NEW.source_id
           AND source.schema_id = NEW.source_schema
           AND source.owner_id = NEW.record_owner;
    ELSIF NEW.record_kind = 'blob' THEN
        NEW.source_pins := '{}';
    END IF;
    IF NEW.record_kind IN ('memory', 'goal') AND NEW.source_pins IS NULL THEN
        RAISE EXCEPTION 'schema evolution authority source does not match its declared schema and owner'
            USING ERRCODE = '23503';
    END IF;
    RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.assert_schema_evolution_authority_drained()
 RETURNS trigger LANGUAGE plpgsql
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
BEGIN
    IF EXISTS (SELECT 1 FROM proxiima_core.schema_evolution_authority
                WHERE xact = pg_current_xact_id()) THEN
        RAISE EXCEPTION 'schema evolution authority permits must be cleared before commit'
            USING ERRCODE = '55000';
    END IF;
    RETURN NULL;
END
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_blob_guard()
 RETURNS trigger LANGUAGE plpgsql
AS $function$
BEGIN
    IF NOT proxiima_core.schema_evolution_mapping_matches('blob', OLD.blob_id, NEW.blob_id, OLD.schema_id, NEW.schema_id, OLD.owner_id) THEN
        IF NEW.blob_id IS DISTINCT FROM OLD.blob_id
           OR NEW.schema_id IS DISTINCT FROM OLD.schema_id OR NEW.content_hash IS DISTINCT FROM OLD.content_hash
        THEN
            RAISE EXCEPTION 'append-only: blob identity cannot be updated' USING ERRCODE = '25006';
        END IF;
    ELSIF NEW.blob_id IS DISTINCT FROM OLD.blob_id OR NEW.owner_id IS DISTINCT FROM OLD.owner_id
       OR NEW.content_hash IS DISTINCT FROM OLD.content_hash
    THEN
        RAISE EXCEPTION 'schema evolution may only change blob schema identity' USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_goal_guard()
 RETURNS trigger LANGUAGE plpgsql
AS $function$
BEGIN
    IF NOT proxiima_core.schema_evolution_mapping_matches(
        'goal', OLD.t, NEW.t,
        OLD.schema_id, NEW.schema_id,
        OLD.owner_id
    ) THEN
        RAISE EXCEPTION 'append-only: goal does not accept UPDATE' USING ERRCODE = '25006';
    END IF;
    IF NEW.owner_id IS DISTINCT FROM OLD.owner_id OR NEW."by" IS DISTINCT FROM OLD."by"
       OR NEW.via IS DISTINCT FROM OLD.via OR NEW.state IS DISTINCT FROM OLD.state
    THEN
        RAISE EXCEPTION 'schema evolution may not alter Goal owner, writer, or state'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.goal_stamp_schema()
 RETURNS trigger LANGUAGE plpgsql
AS $function$
DECLARE
    head_schema text;
BEGIN
    SELECT schema_id INTO head_schema
      FROM proxiima_core.goal_head
     WHERE handle = NEW.handle AND owner_id = NEW.owner_id;
    IF head_schema IS NULL THEN
        -- The owner/head FK owns this refusal; do not replace its diagnostic
        -- when RLS hides a foreign series. Schema nullability still applies.
        RETURN NEW;
    END IF;
    IF NEW.schema_id IS NULL THEN
        NEW.schema_id := head_schema;
    ELSIF NEW.schema_id IS DISTINCT FROM head_schema THEN
        RAISE EXCEPTION 'goal schema must equal goal_head.schema_id'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.goal_schema_head_alignment()
 RETURNS trigger LANGUAGE plpgsql
AS $function$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM proxiima_core.goal_head head
          WHERE head.handle = NEW.handle
           AND head.owner_id = NEW.owner_id AND head.schema_id = NEW.schema_id
    ) THEN
        RAISE EXCEPTION 'goal schema, handle, and owner must equal goal_head'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_replay_guard()
 RETURNS trigger LANGUAGE plpgsql
AS $function$
BEGIN
    IF NOT proxiima_core.schema_evolution_target_matches(
        'goal', OLD.goal_t, NEW.goal_t,
        (SELECT schema_id FROM proxiima_core.goal WHERE t = NEW.goal_t),
        OLD.owner_id
    )
       OR NEW.owner_id IS DISTINCT FROM OLD.owner_id
       OR NEW.request_id IS DISTINCT FROM OLD.request_id
    THEN
        RAISE EXCEPTION 'append-only: goal replay identity does not accept UPDATE'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.schema_evolution_ingest_key_guard()
 RETURNS trigger LANGUAGE plpgsql
AS $function$
BEGIN
    IF NOT proxiima_core.schema_evolution_target_matches(
        'memory', OLD.t, NEW.t,
        (SELECT schema_id FROM proxiima_core.memory /* FORGET-POLICY: includes-forgotten (migration keeps historical receipt aliases) */ WHERE t = NEW.t),
        OLD.owner_id
    )
       OR NEW.owner_id IS DISTINCT FROM OLD.owner_id
       OR NEW.source_id IS DISTINCT FROM OLD.source_id
       OR NEW.ingest_key IS DISTINCT FROM OLD.ingest_key
    THEN
        RAISE EXCEPTION 'append-only: ingest identity does not accept UPDATE'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END
$function$;

-- FORGET-POLICY: includes-forgotten (DROP must preserve every stored admission) applies to every read in this function
CREATE OR REPLACE FUNCTION proxiima_core.assert_fact_schema_drained(
    retired_schema_id text, retiring_tables text[]
) RETURNS void
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    surface text;
BEGIN
    IF current_setting('app.proxiima_scope', true) IS DISTINCT FROM 'platform'
       OR NOT EXISTS (
           SELECT 1 FROM pg_roles r
            WHERE r.rolname = current_user AND NOT r.rolsuper AND NOT r.rolbypassrls
              AND pg_has_role(current_user,
                    (SELECT relowner FROM pg_class
                      WHERE oid = 'proxiima_core.memory'::regclass), 'USAGE')
       ) THEN
        RAISE EXCEPTION 'schema DROP requires the migration role in platform scope'
            USING ERRCODE = '42501';
    END IF;
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'schema DROP requires READ COMMITTED for a fresh writer-fenced proof'
            USING ERRCODE = '25001';
    END IF;
    IF retired_schema_id IS NULL OR btrim(retired_schema_id) = ''
       OR retiring_tables IS NULL OR cardinality(retiring_tables) = 0
       OR array_ndims(retiring_tables) <> 1 THEN
        RAISE EXCEPTION 'schema DROP requires a schema identity and declared sidecar tables'
            USING ERRCODE = '22023';
    END IF;

    -- Conflicts with admission's ROW EXCLUSIVE lock and lasts through the
    -- caller's DDL and commit. VOLATILE PL/pgSQL reads get a fresh snapshot
    -- after this wait under READ COMMITTED.
    LOCK TABLE proxiima_core.memory IN SHARE ROW EXCLUSIVE MODE;
    PERFORM proxiima_core.owner_rls_census_table('proxiima_core', 'memory');
    FOREACH surface IN ARRAY retiring_tables LOOP
        IF surface IS NULL OR surface !~ '^[A-Za-z_][A-Za-z0-9_]*\.[A-Za-z_][A-Za-z0-9_]*$'
           OR NOT EXISTS (
            SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname || '.' || c.relname = surface AND c.relkind IN ('r', 'p')
               AND EXISTS (SELECT 1 FROM proxiima_core.spiice_surface s
                            WHERE s.table_name = surface)
        ) THEN
            RAISE EXCEPTION 'schema DROP sidecar % is not an existing declared table', surface
                USING ERRCODE = '22023';
        END IF;
    END LOOP;
    IF EXISTS (
        SELECT 1 FROM proxiima_core.memory m
         WHERE m.schema_id = retired_schema_id OR m.sidecar_tables && retiring_tables
    ) THEN
        RAISE EXCEPTION 'cannot drop schema %: stored admissions still name the schema or its sidecars',
            retired_schema_id
            USING ERRCODE = '23503',
                  HINT = 'Run the erase-capable release first, with its registered host inverses and sidecars present, then retry this schema-only migration.';
    END IF;
END;
$function$;
REVOKE ALL ON FUNCTION proxiima_core.assert_fact_schema_drained(text, text[]) FROM PUBLIC;

-- FORGET-POLICY: includes-forgotten (sidecar integrity applies to historical representations) applies to every read in this function
CREATE OR REPLACE FUNCTION proxiima_core.assert_declared_sidecar_present() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_ARGV[0];
    present boolean;
    persisted_sidecars text[];
BEGIN
    IF NOT (surface = ANY (NEW.sidecar_tables)) THEN
        RETURN NULL;
    END IF;
    SELECT m.sidecar_tables
      INTO persisted_sidecars
      FROM proxiima_core.memory m
     WHERE m.t = NEW.t;
    IF NOT (surface = ANY (COALESCE(persisted_sidecars, '{}'::text[]))) THEN
        IF proxiima_core.schema_evolution_intermediate_memory_schema_permits(
            NEW.t, NEW.schema_id
        ) THEN
            RETURN NULL;
        END IF;
        RAISE EXCEPTION
            'memory % declares % in memory.sidecar_tables with no row in that table',
            NEW.t, surface
            USING ERRCODE = '23503';
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
CREATE OR REPLACE FUNCTION proxiima_core.assert_erased_pin_target_insert()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    IF pg_trigger_depth() < 2
       OR current_setting('proxiima_core.erased_pin_target_writer', true)
              IS DISTINCT FROM NEW.t::text
    THEN
        RAISE EXCEPTION
            'erased_pin_target is written only by a target deletion trigger'
            USING ERRCODE = '42501';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM proxiima_core.memory m
         WHERE m.t = NEW.t AND m.kind::text = NEW.kind::text
        UNION ALL
        SELECT 1 FROM proxiima_core.goal g
         WHERE g.t = NEW.t AND NEW.kind = 'goal'
    ) THEN
        RAISE EXCEPTION
            'erased_pin_target % must match the live row being deleted', NEW.t
            USING ERRCODE = '23503';
    END IF;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$function$;
REVOKE ALL ON FUNCTION proxiima_core.assert_erased_pin_target_insert() FROM PUBLIC;

-- FORGET-POLICY: includes-forgotten (a forgotten memory keeps its sidecar rows) applies to every read in this function
CREATE OR REPLACE FUNCTION proxiima_core.assert_memory_declares_sidecar() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME;
    memory_t uuid := (to_jsonb(NEW) ->> TG_ARGV[0])::uuid;
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM proxiima_core.memory m
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

CREATE OR REPLACE FUNCTION proxiima_core.assert_owner_rls_census(target_schema text)
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
        PERFORM proxiima_core.owner_rls_census_table(target_schema, base_table);
        counted := counted + 1;
    END LOOP;
    IF counted = 0 THEN
        RAISE EXCEPTION 'assert_owner_rls_census: schema % has no tables', target_schema;
    END IF;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.assert_owner_rls_census(target_schema text) FROM PUBLIC;
COMMENT ON FUNCTION proxiima_core.assert_owner_rls_census(target_schema text) IS 'Raises on the first base table of the schema without ENABLE/FORCE RLS, the closed policy set and the owner-role platform policy.';

-- FORGET-POLICY: includes-forgotten (a forgotten memory keeps its sidecar rows) applies to every read in this function
CREATE OR REPLACE FUNCTION proxiima_core.assert_row_not_still_declared() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_ARGV[0];
    memory_t uuid := (to_jsonb(OLD) ->> TG_ARGV[1])::uuid;
    replaced boolean;
BEGIN
    EXECUTE format('SELECT EXISTS (SELECT 1 FROM %s WHERE %I = $1)', surface, TG_ARGV[1])
       INTO replaced USING memory_t;
    IF replaced AND EXISTS (
        SELECT 1
          FROM proxiima_core.schema_evolution_authority authority
         WHERE authority.xact = pg_current_xact_id()
           AND authority.record_kind = 'memory'
           AND authority.source_id = memory_t
           AND authority.target_id = memory_t
           AND authority.primary_target
    ) THEN
        RETURN NULL;
    END IF;
    IF EXISTS (
        SELECT 1
          FROM proxiima_core.memory m
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

CREATE OR REPLACE FUNCTION proxiima_core.assert_sidecar_stamp_declared()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
DECLARE
    undeclared text;
BEGIN
    -- Catalog lookup observes a concurrent DROP even when a repeatable-read
    -- writer's data snapshot still contains the registration. Admission
    -- already holds the memory writer lock, so the DROP guard cannot pass
    -- until this transaction ends.
    SELECT stamped INTO undeclared
      FROM unnest(NEW.sidecar_tables) AS stamped
     WHERE NOT EXISTS (
               SELECT 1
                 FROM proxiima_core.spiice_surface fs
                WHERE fs.table_name = stamped
                  AND to_regclass(fs.table_name) IS NOT NULL
           )
     LIMIT 1;
    IF undeclared IS NOT NULL THEN
        RAISE EXCEPTION
            'memory.sidecar_tables names %, which no spiice declares in proxiima_core.spiice_surface',
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
CREATE OR REPLACE FUNCTION proxiima_core.cooled_forget_grounding()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
    dependent_ids uuid[];
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    IF NEW.kind = 'fact' THEN
        PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;
    SELECT COALESCE(array_agg(m.t ORDER BY m.t), '{}'::uuid[])
      INTO dependent_ids
      FROM proxiima_core.memory m
     WHERE m.kind <> 'fact'
       AND m.t <> NEW.t
       AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t]);
    PERFORM proxiima_core.lock_pin_targets(ARRAY[NEW.t] || dependent_ids);
    IF EXISTS (
        SELECT 1
          FROM proxiima_core.memory m
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
      FROM proxiima_core.memory m
     WHERE m.kind <> 'fact'
       AND m.t <> NEW.t
       AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t])
     ORDER BY m.t
     FOR UPDATE;
    -- No grounding refusal since #488 S13: a forgotten memory still grounds
    -- what was derived from it. The locks above keep forget serialized with
    -- a concurrent derive over the same targets.
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$function$;
REVOKE ALL ON FUNCTION proxiima_core.cooled_forget_grounding() FROM PUBLIC;

-- Immutability functions

CREATE OR REPLACE FUNCTION proxiima_core.enforce_installation_immutable()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    RAISE EXCEPTION 'write-once: the installation identity cannot be changed or removed'
        USING ERRCODE = '25006';
END;
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.enforce_publication_immutable()
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

CREATE OR REPLACE FUNCTION proxiima_core.enforce_publication_origin_immutable()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    RAISE EXCEPTION 'append-only: publication_origin identity cannot be updated'
        USING ERRCODE = '25006';
END;
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.enforce_row_append_only()
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
CREATE OR REPLACE FUNCTION proxiima_core.embedding_heads_deleted_index_debt() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    INSERT INTO proxiima_core.vector_index_debt (entity_id, model_id, dim, owner_id, op)
    SELECT entity_id, model_id, dim, owner_id, 'delete' FROM deleted_heads
    ON CONFLICT (entity_id, model_id, dim) DO UPDATE
        SET owner_id = EXCLUDED.owner_id, op = EXCLUDED.op, enqueued_at = now(),
            xact = pg_current_xact_id();
    RETURN NULL;
END;
$$;

CREATE OR REPLACE FUNCTION proxiima_core.embedding_head_moved_index_debt() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    INSERT INTO proxiima_core.vector_index_debt (entity_id, model_id, dim, owner_id, op)
    VALUES (NEW.entity_id, NEW.model_id, NEW.dim, NEW.owner_id, 'owner')
    ON CONFLICT (entity_id, model_id, dim) DO UPDATE
        SET owner_id = EXCLUDED.owner_id, op = EXCLUDED.op, enqueued_at = now(),
            xact = pg_current_xact_id();
    RETURN NULL;
END;
$$;

-- Erasure functions

CREATE OR REPLACE FUNCTION proxiima_core.erased_pin_target_append_only()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    RAISE EXCEPTION 'erased_pin_target is append-only'
        USING ERRCODE = '25006';
END;
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.record_erased_pin_target(target uuid, target_kind proxiima_core.pin_target_kind)
 RETURNS void
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    existing_kind proxiima_core.pin_target_kind;
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    PERFORM proxiima_core.lock_pin_targets(ARRAY[target]);
    SELECT kind INTO existing_kind
      FROM proxiima_core.erased_pin_target
     WHERE t = target;
    IF FOUND THEN
        IF existing_kind <> target_kind THEN
            RAISE EXCEPTION
                'erased pin target % already records kind %, not %',
                target, existing_kind, target_kind
                USING ERRCODE = '23505';
        END IF;
        PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
        RETURN;
    END IF;
    PERFORM set_config('proxiima_core.erased_pin_target_writer', target::text, true);
    INSERT INTO proxiima_core.erased_pin_target (t, kind)
    VALUES (target, target_kind);
    PERFORM set_config('proxiima_core.erased_pin_target_writer', '', true);
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
END;
$function$;
REVOKE ALL ON FUNCTION proxiima_core.record_erased_pin_target(target uuid, target_kind proxiima_core.pin_target_kind) FROM PUBLIC;

-- Goal functions

CREATE OR REPLACE FUNCTION proxiima_core.goal_erase_witness()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    IF EXISTS (
        SELECT 1
          FROM proxiima_core.goal target
         WHERE target.owner_id = OLD.owner_id
           AND proxiima_core.schema_evolution_mapping_matches(
               'goal', OLD.t, target.t,
               OLD.schema_id, target.schema_id, OLD.owner_id)
    ) THEN
        PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
        RETURN OLD;
    END IF;
    PERFORM proxiima_core.lock_pin_targets(ARRAY[OLD.t]);
    PERFORM proxiima_core.record_erased_pin_target(OLD.t, 'goal');
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN OLD;
END;
$function$;
REVOKE ALL ON FUNCTION proxiima_core.goal_erase_witness() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.goal_head_t_only()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    IF proxiima_core.schema_evolution_mapping_matches('goal', OLD.t, NEW.t, OLD.schema_id, NEW.schema_id, OLD.owner_id)
       AND proxiima_core.schema_evolution_head_permits('goal', OLD.t, NEW.t, OLD.handle, NEW.handle) THEN
        IF NEW.handle IS DISTINCT FROM OLD.handle OR NEW.owner_id IS DISTINCT FROM OLD.owner_id
           OR NEW.authority_generation < OLD.authority_generation
        THEN
            RAISE EXCEPTION 'schema evolution may not alter Goal handle, owner, or authority generation'
                USING ERRCODE = '25006';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
       OR NEW.owner_id IS DISTINCT FROM OLD.owner_id
       OR NEW.authority_generation IS DISTINCT FROM OLD.authority_generation THEN
        RAISE EXCEPTION 'goal_head is frozen except t'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.goal_no_later_after_terminal()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    IF EXISTS (
        SELECT 1 FROM proxiima_core.goal source
         WHERE source.handle = NEW.handle AND source.owner_id = NEW.owner_id
           AND source.state = NEW.state
           AND proxiima_core.schema_evolution_mapping_matches(
               'goal', source.t, NEW.t, source.schema_id, NEW.schema_id, source.owner_id)
    ) THEN
        RETURN NEW;
    END IF;
    IF EXISTS (
        SELECT 1 FROM proxiima_core.goal g
         WHERE g.handle = NEW.handle
           AND g.state IN ('Achieved', 'Abandoned')
           AND g.t <> NEW.t
    ) THEN
        RAISE EXCEPTION 'terminal goal admits no later t' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.goal_parent_fixed()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    -- #514: a series keeps the parent it was decomposed under.
    IF EXISTS (
        SELECT 1
          FROM proxiima_core.goal prior
          JOIN proxiima_core.goal_head target_head ON target_head.handle = NEW.handle
         WHERE prior.handle = NEW.handle
           AND proxiima_core.schema_evolution_mapping_matches(
               'goal', prior.t, NEW.t, prior.schema_id, target_head.schema_id, prior.owner_id)
    ) THEN
        RETURN NEW;
    END IF;
    IF EXISTS (
        SELECT 1 FROM proxiima_core.goal g
         WHERE g.handle = NEW.handle
           AND g.parent_handle IS DISTINCT FROM NEW.parent_handle
    ) THEN
        RAISE EXCEPTION 'goal series parent is fixed' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$function$;

-- FORGET-POLICY: includes-forgotten (pin integrity holds for forgotten targets) applies to every read in this function
CREATE OR REPLACE FUNCTION proxiima_core.goal_pin_target_checks()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
    pin uuid;
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    PERFORM proxiima_core.lock_pin_targets(
        array_remove(
            ARRAY[NEW.t, NEW.close_fact_t, NEW.assignment_t, NEW.write_act_t],
            NULL
        ) || NEW.dependency_t || NEW.evidence_t || NEW.motivated_by
    );
    SELECT e.t INTO pin
      FROM proxiima_core.erased_pin_target e
     WHERE e.t = ANY (
         array_remove(
             ARRAY[NEW.close_fact_t, NEW.assignment_t, NEW.write_act_t], NULL
         ) || NEW.dependency_t || NEW.evidence_t || NEW.motivated_by
     )
       AND NOT proxiima_core.schema_evolution_existing_pin_permits('goal', NEW.t, e.t)
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'goal declaration names erased target %', pin
            USING ERRCODE = '23503';
    END IF;
    IF EXISTS (SELECT 1 FROM proxiima_core.erased_pin_target WHERE t = NEW.t)
       OR EXISTS (SELECT 1 FROM proxiima_core.memory WHERE t = NEW.t)
    THEN
        RAISE EXCEPTION 'goal t % collides with an existing entity', NEW.t
            USING ERRCODE = '23505';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM unnest(NEW.motivated_by) AS p(id)
          LEFT JOIN proxiima_core.memory m ON m.t = p.id
         WHERE (m.t IS NULL OR m.kind <> 'perspective'::proxiima_core.memory_kind)
           AND NOT (m.t IS NULL AND EXISTS (
               SELECT 1 FROM proxiima_core.erased_pin_target e
                WHERE e.t=p.id AND e.kind='perspective'
                  AND proxiima_core.schema_evolution_existing_pin_permits('goal', NEW.t, p.id)
           ))
    ) THEN
        RAISE EXCEPTION 'goal motivation targets must be Perspective versions'
            USING ERRCODE = '23514';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM unnest(NEW.motivated_by) AS p(id)
          JOIN proxiima_core.memory m ON m.t = p.id
         WHERE m.owner_id <> NEW.owner_id
    ) THEN
        RAISE EXCEPTION 'goal motivation target owner differs from Goal owner'
            USING ERRCODE = '23514';
    END IF;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$function$;
REVOKE ALL ON FUNCTION proxiima_core.goal_pin_target_checks() FROM PUBLIC;

-- Group functions

-- The platform gateway acquires all trees before these ordered kind and
-- projection keys. Keep the key construction beside the membership writers.
CREATE OR REPLACE FUNCTION proxiima_core.lock_authority_targets(p_owners uuid[], p_subjects uuid[])
 RETURNS void
 LANGUAGE plpgsql VOLATILE SECURITY INVOKER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    target uuid;
BEGIN
    FOR target IN SELECT DISTINCT id FROM unnest(p_owners) AS ids(id) ORDER BY id
    LOOP
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxiima-owner-kind:' || target::text, 0));
    END LOOP;
    FOR target IN SELECT DISTINCT id FROM unnest(p_subjects) AS ids(id) ORDER BY id
    LOOP
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxiima-group-projection:' || target::text, 0));
    END LOOP;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.lock_authority_targets(uuid[], uuid[]) FROM PUBLIC;

-- The authenticated subject's authority projection is distinct from memory
-- owner reach. VOLATILE gives the query after a lock wait a fresh snapshot.
CREATE OR REPLACE FUNCTION proxiima_core.read_subject_authority(p_subject uuid)
 RETURNS TABLE(group_id uuid, read_ceiling proxiima_core.access_ceiling,
               write_ceiling proxiima_core.access_ceiling, manage boolean)
 LANGUAGE plpgsql VOLATILE SECURITY INVOKER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_subject text := current_setting('app.authority_subject', true);
BEGIN
    IF current_setting('app.proxiima_scope', true) IS DISTINCT FROM 'owner'
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
        hashtextextended('proxiima-owner-kind:' || p_subject::text, 0));
    PERFORM pg_advisory_xact_lock_shared(
        hashtextextended('proxiima-group-projection:' || p_subject::text, 0));
    PERFORM set_config('app.authority_subject', p_subject::text, true);
    IF EXISTS (SELECT 1 FROM proxiima_core.owners o
                WHERE o.owner_id = p_subject AND o.kind = 'group') THEN
        RAISE EXCEPTION USING ERRCODE = '42501',
            MESSAGE = 'authority subject must be a person';
    END IF;
    RETURN QUERY
        SELECT m.group_id, m.read_ceiling, m.write_ceiling, m.manage
          FROM proxiima_core.group_role_projection m
         WHERE m.member_user_id = p_subject
         ORDER BY m.group_id;
    PERFORM set_config('app.authority_subject', COALESCE(previous_subject, ''), true);
EXCEPTION WHEN OTHERS THEN
    PERFORM set_config('app.authority_subject', COALESCE(previous_subject, ''), true);
    RAISE;
END
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.group_edge_before_write()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    IF TG_OP = 'DELETE' THEN
        PERFORM proxiima_core.lock_group_tree(OLD.child_group_id);
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxiima-owner-kind:' || OLD.child_group_id::text, 0));
        PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
        RETURN OLD;
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.child_group_id, NEW.parent_group_id) <> (OLD.child_group_id, OLD.parent_group_id) THEN
        RAISE EXCEPTION 'group edge identity is immutable';
    END IF;
    PERFORM proxiima_core.lock_group_trees(NEW.child_group_id, NEW.parent_group_id);
    IF NEW.child_group_id <= NEW.parent_group_id THEN
        PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-owner-kind:' || NEW.child_group_id::text, 0));
        PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-owner-kind:' || NEW.parent_group_id::text, 0));
    ELSE
        PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-owner-kind:' || NEW.parent_group_id::text, 0));
        PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-owner-kind:' || NEW.child_group_id::text, 0));
    END IF;
    IF NEW.child_group_id = NEW.parent_group_id THEN
        RAISE EXCEPTION 'a group cannot contain itself';
    END IF;
    IF EXISTS (
        WITH RECURSIVE descendants(group_id) AS (
            SELECT NEW.parent_group_id
          UNION ALL SELECT e.parent_group_id FROM descendants d
            JOIN proxiima_core.group_edges e ON e.child_group_id = d.group_id
        ) SELECT 1 FROM descendants WHERE group_id = NEW.child_group_id
    ) THEN
        RAISE EXCEPTION 'group edge would create a cycle';
    END IF;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.group_edge_before_write() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.group_lock_request_cleanup()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    DELETE FROM proxiima_core.group_lock_request WHERE request_id = NEW.request_id;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN NULL;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.group_lock_request_cleanup() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.group_lock_request_trigger()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    IF TG_OP = 'INSERT' THEN
        IF NEW.right_group_id IS NULL THEN
            PERFORM proxiima_core.lock_group_tree(NEW.left_group_id);
        ELSE
            PERFORM proxiima_core.lock_group_trees(NEW.left_group_id, NEW.right_group_id);
        END IF;
        IF NEW.right_group_id IS NULL THEN
            NEW.member_is_group := false;
        ELSE
            IF NEW.left_group_id <= NEW.right_group_id THEN
                PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-owner-kind:' || NEW.left_group_id::text, 0));
                PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-owner-kind:' || NEW.right_group_id::text, 0));
            ELSE
                PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-owner-kind:' || NEW.right_group_id::text, 0));
                PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-owner-kind:' || NEW.left_group_id::text, 0));
            END IF;
            SELECT EXISTS (
                SELECT 1 FROM proxiima_core.owners
                 WHERE owner_id = NEW.right_group_id AND kind = 'group'
            ) INTO NEW.member_is_group;
        END IF;
        PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;
    RETURN OLD;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.group_lock_request_trigger() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.group_membership_before_write()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    member_kind proxiima_core.owner_kind;
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    PERFORM proxiima_core.lock_group_tree(COALESCE(NEW.group_id, OLD.group_id));
    IF TG_OP = 'DELETE' THEN
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxiima-owner-kind:' || OLD.member_user_id::text, 0));
        PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
        RETURN OLD;
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.group_id, NEW.member_user_id) <> (OLD.group_id, OLD.member_user_id) THEN
        RAISE EXCEPTION 'membership identity is immutable';
    END IF;
    PERFORM pg_advisory_xact_lock(
        hashtextextended('proxiima-owner-kind:' || NEW.member_user_id::text, 0));
    SELECT kind INTO member_kind FROM proxiima_core.owners WHERE owner_id = NEW.member_user_id;
    IF member_kind = 'group' THEN
        RAISE EXCEPTION 'owner % is a group; use the group edge write', NEW.member_user_id;
    END IF;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.group_membership_before_write() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.group_projection_after_write()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    users uuid[];
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    IF TG_TABLE_NAME = 'group_memberships' THEN
        users := ARRAY[COALESCE(NEW.member_user_id, OLD.member_user_id)];
    ELSE
        WITH RECURSIVE descendants(group_id) AS (
            SELECT COALESCE(NEW.child_group_id, OLD.child_group_id)
          UNION ALL SELECT e.child_group_id FROM descendants d
            JOIN proxiima_core.group_edges e ON e.parent_group_id = d.group_id
        )
        SELECT COALESCE(array_agg(DISTINCT m.member_user_id), '{}') INTO users
          FROM descendants d JOIN proxiima_core.group_memberships m ON m.group_id = d.group_id;
    END IF;
    IF cardinality(users) > 0 THEN PERFORM proxiima_core.rebuild_group_role_projection(users); END IF;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN NULL;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.group_projection_after_write() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.group_projection_write_guard()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
BEGIN
    IF current_setting('proxiima.projection_maintenance', true) IS DISTINCT FROM 'on'
       OR pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION 'group_role_projection is maintained by membership triggers';
    END IF;
    RETURN COALESCE(NEW, OLD);
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.group_projection_write_guard() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.group_tree_root(p_group uuid)
 RETURNS uuid
 LANGUAGE sql
 STABLE SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
    WITH RECURSIVE parents(group_id, depth) AS (
        SELECT p_group, 0
      UNION ALL
        SELECT e.parent_group_id, p.depth + 1
          FROM parents p JOIN proxiima_core.group_edges e
            ON e.child_group_id = p.group_id
    )
    SELECT group_id FROM parents ORDER BY depth DESC LIMIT 1
$function$;
REVOKE ALL ON FUNCTION proxiima_core.group_tree_root(p_group uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.group_truncate_guard()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
BEGIN
    RAISE EXCEPTION '% cannot be truncated; membership projection requires row triggers', TG_TABLE_NAME;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.group_truncate_guard() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.rebuild_group_role_projection(p_users uuid[])
 RETURNS void
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    user_row record;
    user_id uuid;
    previous_scope text := current_setting('app.proxiima_scope', true);
    previous_maintenance text := current_setting('proxiima.projection_maintenance', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    PERFORM set_config('proxiima.projection_maintenance', 'on', true);
    FOR user_row IN
        SELECT DISTINCT u AS user_id
          FROM unnest(COALESCE(p_users, '{}')) AS u
         ORDER BY u
    LOOP
        user_id := user_row.user_id;
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxiima-group-projection:' || user_id::text, 0));
        DELETE FROM proxiima_core.group_role_projection WHERE member_user_id = user_id;
        INSERT INTO proxiima_core.group_role_projection
        (member_user_id, group_id, read_ceiling, write_ceiling, manage)
    WITH RECURSIVE relation_roles(relation, read_rank, write_rank, manages) AS (
        VALUES ('admin'::proxiima_core.membership_relation, 4, 4, true),
               ('editor'::proxiima_core.membership_relation, 4, 3, false),
               ('viewer'::proxiima_core.membership_relation, 4, 0, false),
               ('ingest'::proxiima_core.membership_relation, 1, 1, false)
    ), paths(member_user_id, group_id, read_rank, write_rank, manages) AS (
        SELECT m.member_user_id, m.group_id,
               r.read_rank, r.write_rank, r.manages
          FROM proxiima_core.group_memberships m
          JOIN relation_roles r ON r.relation = m.relation
         WHERE m.member_user_id = user_id
        UNION ALL
        SELECT p.member_user_id, e.parent_group_id,
               LEAST(p.read_rank, r.read_rank),
               LEAST(p.write_rank, r.write_rank),
               p.manages AND r.manages
          FROM paths p JOIN proxiima_core.group_edges e ON e.child_group_id = p.group_id
          JOIN relation_roles r ON r.relation = e.relation
    ), folded AS (
        SELECT member_user_id, group_id, max(read_rank) AS read_rank,
               max(write_rank) AS write_rank, bool_or(manages) AS manages
          FROM paths GROUP BY member_user_id, group_id
    )
    SELECT member_user_id, group_id,
           (ARRAY['none','fact','abstraction','perspective','goal'])[read_rank + 1]::proxiima_core.access_ceiling,
           (ARRAY['none','fact','abstraction','perspective','goal'])[write_rank + 1]::proxiima_core.access_ceiling,
           manages
      FROM folded;
    END LOOP;
    PERFORM set_config('proxiima.projection_maintenance', COALESCE(previous_maintenance, ''), true);
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.rebuild_group_role_projection(p_users uuid[]) FROM PUBLIC;

-- Owner-RLS installation functions

CREATE OR REPLACE FUNCTION proxiima_core.install_owner_rls(target_schema text, owner_id_tables text[], fk_parent_tables text[], ownerless_tables text[], memory_owner_tables text[] DEFAULT '{}'::text[])
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
    class proxiima_core.owner_rls_class;
    classes integer;
    installed integer := 0;
    follows text[];
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    IF target_schema IS NULL OR target_schema IN ('proxiima_core', 'public', 'information_schema')
       OR target_schema LIKE 'pg\_%' THEN
        RAISE EXCEPTION 'install_owner_rls: % is not a spiice schema', COALESCE(target_schema, 'NULL');
    END IF;
    IF to_regnamespace(quote_ident(target_schema)) IS NULL THEN
        RAISE EXCEPTION 'install_owner_rls: schema % does not exist', target_schema;
    END IF;
    -- The host's Follows tables of this schema (sync_owner_follows): each
    -- gets the follows rule whatever class it is listed in, and needs no
    -- listing at all.
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    SELECT COALESCE(array_agg(substr(f.table_name, length(target_schema) + 2)), '{}')
      INTO follows
      FROM proxiima_core.owner_follows AS f
     WHERE split_part(f.table_name, '.', 1) = target_schema;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
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
            proxiima_core.owner_rls_parent_fk('install_owner_rls', target_schema, relation.relname));
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
            PERFORM proxiima_core.owner_rls_apply_follows(
                'install_owner_rls', relation.schema_name, relation.relname);
            installed := installed + 1;
            CONTINUE;
        END IF;
        PERFORM proxiima_core.owner_rls_apply_class(
            'install_owner_rls', 'in ' || class::text || '_tables',
            relation.schema_name, relation.relname, class, 'owner_id');
        installed := installed + 1;
    END LOOP;

    IF installed = 0 THEN
        RAISE EXCEPTION 'install_owner_rls: schema % has no tables', target_schema;
    END IF;

    PERFORM proxiima_core.assert_owner_rls_census(target_schema);
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.install_owner_rls(target_schema text, owner_id_tables text[], fk_parent_tables text[], ownerless_tables text[], memory_owner_tables text[]) FROM PUBLIC;
COMMENT ON FUNCTION proxiima_core.install_owner_rls(target_schema text, owner_id_tables text[], fk_parent_tables text[], ownerless_tables text[], memory_owner_tables text[]) IS 'Owner-RLS policies for one spiice schema from its table classification; refuses an unclassified table. A table in proxiima_core.owner_follows gets the host follows rule whatever class it is listed in, and may be left out (docs/build-a-spiice.md §2, docs/host-integration.md §Boot a composed host).';

CREATE OR REPLACE FUNCTION proxiima_core.install_owner_rls_table(target_schema text, target_table text, class proxiima_core.owner_rls_class, owner_column text DEFAULT 'owner_id'::text)
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
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    IF target_schema IS NULL OR target_schema IN ('proxiima_core', 'public', 'information_schema')
       OR target_schema LIKE 'pg\_%' THEN
        RAISE EXCEPTION 'install_owner_rls_table: % is not a spiice schema', COALESCE(target_schema, 'NULL');
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
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    SELECT EXISTS (
        SELECT 1 FROM proxiima_core.owner_follows AS f
         WHERE f.table_name = target_schema || '.' || target_table
    ) INTO follows;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    IF follows THEN
        RAISE NOTICE 'install_owner_rls_table: %.% is a Follows table; the host installs Follows owner RLS, so its class % is superseded',
            target_schema, target_table, class;
        PERFORM proxiima_core.owner_rls_apply_follows('install_owner_rls_table', target_schema, target_table);
        PERFORM proxiima_core.owner_rls_census_table(target_schema, target_table);
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
                RAISE EXCEPTION 'install_owner_rls_table: %.%.% is %, not uuid; an owner column holds a proxiima_core.owners id, so store OwnerRef::stable_key_uuid() in a uuid column and keep the personal:<uuid> / group:<uuid> string at the edge',
                    target_schema, target_table, owner_column, format_type(column_type, NULL);
            END IF;
        WHEN 'fk_parent' THEN
            -- The chain of parents must not return to this table: the
            -- policies would recurse on every read. Only the parents are
            -- known, so follow them through every in-schema table without an
            -- owner_id column; proxiima_core, a table with an owner column and
            -- a parent without a resolvable FK end the chain.
            hop := proxiima_core.owner_rls_parent_fk('install_owner_rls_table', target_schema, target_table);
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
                hop := proxiima_core.owner_rls_find_parent_fk(target_schema, next_table);
                EXIT WHEN hop IS NULL OR (NOT (hop ->> 'on_key')::boolean AND (hop ->> 'candidates')::integer > 1);
            END LOOP;
        WHEN 'ownerless' THEN
            NULL;
        WHEN 'memory_owner' THEN
            NULL;
    END CASE;

    PERFORM proxiima_core.owner_rls_apply_class(
        'install_owner_rls_table', format('class %s', class),
        target_schema, target_table, class, owner_column);
    PERFORM proxiima_core.owner_rls_census_table(target_schema, target_table);
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.install_owner_rls_table(target_schema text, target_table text, class proxiima_core.owner_rls_class, owner_column text) FROM PUBLIC;
COMMENT ON FUNCTION proxiima_core.install_owner_rls_table(target_schema text, target_table text, class proxiima_core.owner_rls_class, owner_column text) IS 'Owner-RLS policies for one table of a spiice or host schema; the per-table form of install_owner_rls. A table in proxiima_core.owner_follows gets the host follows rule instead of class owner_id or memory_owner (docs/build-a-spiice.md §2, docs/host-integration.md §Boot a composed host).';

-- Host only: record the Follows tables of the schemas this binary composes
-- and install their owner RLS. The host calls it from its Rust registry
-- (pg_sidecar!(owner: follows)) in the migration lane: once before the
-- spiice sources, when a table may not exist yet and is only marked, and once
-- after them. Marks of other schemas are left alone, so binaries composing
-- different bundles over one database do not undo each other.
CREATE OR REPLACE FUNCTION proxiima_core.sync_owner_follows(schemas text[], tables text[])
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
    IF current_setting('app.proxiima_scope', true) IS DISTINCT FROM 'platform' THEN
        RAISE EXCEPTION 'sync_owner_follows: runs in platform scope only';
    END IF;
    IF schemas IS NULL OR tables IS NULL THEN
        RAISE EXCEPTION 'sync_owner_follows: schemas and tables must not be NULL';
    END IF;
    FOREACH rel_schema IN ARRAY schemas LOOP
        IF rel_schema IS NULL OR rel_schema IN ('proxiima_core', 'public', 'information_schema')
           OR rel_schema LIKE 'pg\_%' THEN
            RAISE EXCEPTION 'sync_owner_follows: % is not a spiice schema', COALESCE(rel_schema, 'NULL');
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

    DELETE FROM proxiima_core.owner_follows AS f
     WHERE split_part(f.table_name, '.', 1) = ANY(schemas)
       AND f.table_name <> ALL(tables);
    INSERT INTO proxiima_core.owner_follows (table_name)
    SELECT DISTINCT unnest(tables)
    ON CONFLICT DO NOTHING;

    FOR listed IN SELECT DISTINCT unnest(tables) ORDER BY 1 LOOP
        rel_schema := split_part(listed, '.', 1);
        rel_name := split_part(listed, '.', 2);
        SELECT c.oid INTO rel_oid
          FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = rel_schema AND c.relname = rel_name AND c.relkind IN ('r', 'p');
        -- Not created yet: the spiice lane creates it, its installer run
        -- applies the follows rule, and the post-lane sync adds the index.
        CONTINUE WHEN rel_oid IS NULL;

        PERFORM proxiima_core.owner_rls_apply_follows('sync_owner_follows', rel_schema, rel_name);

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

        PERFORM proxiima_core.owner_rls_census_table(rel_schema, rel_name);
    END LOOP;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.sync_owner_follows(schemas text[], tables text[]) FROM PUBLIC;
COMMENT ON FUNCTION proxiima_core.sync_owner_follows(schemas text[], tables text[]) IS 'Host only: replaces the owner_follows marks of the composed schemas with the registry Follows tables, and gives each existing one the follows owner RLS (owner_id AND EXISTS over its memory) and a btree index leading with owner_id.';

-- Lexical functions

CREATE OR REPLACE FUNCTION proxiima_core.lexical_language_forget(config_to_forget regconfig)
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
    IF config_to_forget = proxiima_core.lexical_config() THEN
        RAISE EXCEPTION 'cannot forget %: it is the default lexical configuration',
            config_to_forget;
    END IF;

    BEGIN
        DELETE FROM proxiima_core.lexical_languages
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
COMMENT ON FUNCTION proxiima_core.lexical_language_forget(config_to_forget regconfig) IS 'Remove a configuration from the active-language set, refusing while any row still holds it in an FK-stamped lexical_language column. The FK checks serialize this against in-flight writes. Run this BEFORE dropping a custom text search configuration: PostgreSQL allows the drop with rows still referencing it, and those rows are then un-updatable (cache lookup failed on the dangling OID).';

CREATE OR REPLACE FUNCTION proxiima_core.lexical_query_text(config regconfig, query_text text)
 RETURNS text
 LANGUAGE sql
 STABLE PARALLEL SAFE
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
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
        SELECT l.config FROM proxiima_core.lexical_languages l
        UNION ALL
        SELECT config WHERE NOT EXISTS (SELECT 1 FROM proxiima_core.lexical_languages)
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
                   left(query_text, t.end_position::integer) || ' proxiimaoperatorprobe')::text AS with_or,
               websearch_to_tsquery('pg_catalog.simple'::regconfig,
                   left(query_text, (t.end_position - length(t.token))::integer)
                       || ' proxiimaoperatorprobe')::text AS without_or
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
                       || 'proxiimaoperatorprobe')::text, '!''proxiimaoperatorprobe''') > 0
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
                       || 'proxiimaoperatorprobe')::text, '!''proxiimaoperatorprobe''') > 0
               THEN ltrim(t.token, '-')
               ELSE t.token END,
               '' ORDER BY t.ordinal
           ), query_text)
      FROM query_rendered_tokens t
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.lexical_scrub(txt text)
 RETURNS text
 LANGUAGE sql
 IMMUTABLE PARALLEL SAFE STRICT
AS $function$ SELECT regexp_replace(
       regexp_replace(txt, '[[:punct:]]+', ' ', 'g'),
       '\m[[:alnum:]]{255}[[:alnum:]]+\M', ' ', 'g') $function$;

CREATE OR REPLACE FUNCTION proxiima_core.lexical_tsv(txt text)
 RETURNS tsvector
 LANGUAGE sql
 STABLE PARALLEL SAFE STRICT
AS $function$ SELECT to_tsvector(proxiima_core.lexical_config(), proxiima_core.lexical_scrub(txt)) $function$;

CREATE OR REPLACE FUNCTION proxiima_core.lexical_tsv(config regconfig, txt text)
 RETURNS tsvector
 LANGUAGE sql
 IMMUTABLE PARALLEL SAFE STRICT
AS $function$ SELECT to_tsvector(config, proxiima_core.lexical_scrub(txt)) $function$;

CREATE OR REPLACE FUNCTION proxiima_core.remember_lexical_language()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    INSERT INTO proxiima_core.lexical_languages (config)
    VALUES (NEW.lexical_language)
    ON CONFLICT DO NOTHING;
    RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.set_lexical_config(cfg text)
 RETURNS void
 LANGUAGE sql
AS $function$ INSERT INTO proxiima_core.lexical_languages (config)
   VALUES (cfg::regconfig)
   ON CONFLICT DO NOTHING;
   INSERT INTO proxiima_core.lexical_default (singleton, config)
   VALUES (true, cfg::regconfig)
   ON CONFLICT (singleton) DO UPDATE
   SET config = EXCLUDED.config $function$;

-- Lock functions

CREATE OR REPLACE FUNCTION proxiima_core.lock_group_tree(p_group uuid)
 RETURNS void
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    before_root uuid;
    after_root uuid;
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING
            ERRCODE = '25001',
            MESSAGE = 'nested group membership writes require READ COMMITTED';
    END IF;
    LOOP
        before_root := proxiima_core.group_tree_root(p_group);
        before_root := COALESCE(before_root, p_group);
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxiima-group-tree:' || before_root::text, 0));
        after_root := proxiima_core.group_tree_root(p_group);
        after_root := COALESCE(after_root, p_group);
        EXIT WHEN before_root = after_root;
    END LOOP;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.lock_group_tree(p_group uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.lock_group_trees(p_left uuid, p_right uuid)
 RETURNS void
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    left_root uuid;
    right_root uuid;
    checked_left uuid;
    checked_right uuid;
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING ERRCODE = '25001',
            MESSAGE = 'nested group membership writes require READ COMMITTED';
    END IF;
    LOOP
        left_root := proxiima_core.group_tree_root(p_left);
        right_root := proxiima_core.group_tree_root(p_right);
        left_root := COALESCE(left_root, p_left);
        right_root := COALESCE(right_root, p_right);
        IF left_root <= right_root THEN
            PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-group-tree:' || left_root::text, 0));
            IF left_root <> right_root THEN
                PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-group-tree:' || right_root::text, 0));
            END IF;
        ELSE
            PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-group-tree:' || right_root::text, 0));
            PERFORM pg_advisory_xact_lock(hashtextextended('proxiima-group-tree:' || left_root::text, 0));
        END IF;
        checked_left := proxiima_core.group_tree_root(p_left);
        checked_right := proxiima_core.group_tree_root(p_right);
        EXIT WHEN checked_left = left_root AND checked_right = right_root;
    END LOOP;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.lock_group_trees(p_left uuid, p_right uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.lock_pin_targets(targets uuid[])
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
            hashtextextended('proxiima-forget:' || target::text, 0)
        );
    END LOOP;
END;
$function$;

-- Memory functions

CREATE OR REPLACE FUNCTION proxiima_core.memory_align_head()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
DECLARE
    head_kind proxiima_core.memory_kind;
    head_owner uuid;
    head_schema text;
BEGIN
    SELECT kind, owner_id, schema_id INTO head_kind, head_owner, head_schema
      FROM proxiima_core.memory_head
     WHERE handle = NEW.handle;
    IF head_kind IS DISTINCT FROM NEW.kind
       OR head_owner IS DISTINCT FROM NEW.owner_id
       OR head_schema IS DISTINCT FROM NEW.schema_id THEN
        IF proxiima_core.schema_evolution_intermediate_memory_image_permits(
            NEW.t, NEW.schema_id, head_schema, NEW.handle, NEW.kind, NEW.owner_id
        ) THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'memory kind/owner/schema must equal memory_head'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.memory_erase_witness()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    IF EXISTS (
        SELECT 1
          FROM proxiima_core.memory target /* FORGET-POLICY: includes-forgotten (a forgotten replacement remains a migration target) */
         WHERE target.owner_id = OLD.owner_id
           AND proxiima_core.schema_evolution_mapping_matches(
               'memory', OLD.t, target.t, OLD.schema_id, target.schema_id, OLD.owner_id)
    ) THEN
        PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
        RETURN OLD;
    END IF;
    DELETE FROM proxiima_core.publication_outbox outbox
     USING proxiima_core.schema_evolution_publication link
    WHERE link.memory_t = OLD.t AND outbox.t = link.publication_t;
    DELETE FROM proxiima_core.publication_origin origin
     USING proxiima_core.schema_evolution_publication link
    WHERE link.memory_t = OLD.t AND origin.t = link.publication_t;
    PERFORM proxiima_core.lock_pin_targets(ARRAY[OLD.t]);
    -- Forget keeps the row (#488), so deleting a memory row is always an
    -- erase and always leaves a witness.
    PERFORM proxiima_core.record_erased_pin_target(
        OLD.t, OLD.kind::text::proxiima_core.pin_target_kind
    );
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN OLD;
END;
$function$;
REVOKE ALL ON FUNCTION proxiima_core.memory_erase_witness() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.memory_head_t_only()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    IF proxiima_core.schema_evolution_mapping_matches('memory', OLD.t, NEW.t, OLD.schema_id, NEW.schema_id, OLD.owner_id)
       AND proxiima_core.schema_evolution_head_permits('memory', OLD.t, NEW.t, OLD.handle, NEW.handle) THEN
        IF NEW.handle IS DISTINCT FROM OLD.handle OR NEW.kind IS DISTINCT FROM OLD.kind
           OR NEW.owner_id IS DISTINCT FROM OLD.owner_id
           OR NEW.authority_generation < OLD.authority_generation
           OR NEW.publication_sequence < OLD.publication_sequence
        THEN
            RAISE EXCEPTION 'schema evolution may not alter memory head identity or reduce generations'
                USING ERRCODE = '25006';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.owner_id IS DISTINCT FROM OLD.owner_id THEN
        NEW.authority_generation := OLD.authority_generation + 1;
    END IF;
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
       OR (NEW.owner_id IS NOT DISTINCT FROM OLD.owner_id
           AND NEW.authority_generation IS DISTINCT FROM OLD.authority_generation) THEN
        RAISE EXCEPTION 'memory_head is frozen except t, owner_id and authority_generation'
            USING ERRCODE = '25006';
    END IF;
    IF NEW.publication_sequence < OLD.publication_sequence THEN
        RAISE EXCEPTION 'memory_head.publication_sequence never decreases'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.memory_owner_or_append_only()
 RETURNS trigger
 LANGUAGE plpgsql
AS $function$
BEGIN
    IF proxiima_core.schema_evolution_mapping_matches('memory', OLD.t, NEW.t, OLD.schema_id, NEW.schema_id, OLD.owner_id)
       AND proxiima_core.schema_evolution_head_permits('memory', OLD.t, NEW.t, OLD.handle, NEW.handle) THEN
        IF NEW.t IS DISTINCT FROM OLD.t OR NEW.kind IS DISTINCT FROM OLD.kind
           OR NEW.owner_id IS DISTINCT FROM OLD.owner_id OR NEW."by" IS DISTINCT FROM OLD."by"
           OR NEW.via IS DISTINCT FROM OLD.via OR NEW.occurred_at IS DISTINCT FROM OLD.occurred_at
        THEN
            RAISE EXCEPTION 'schema evolution may not alter memory identity, ownership, writer, or occurrence'
                USING ERRCODE = '25006';
        END IF;
        RETURN NEW;
    END IF;
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
       OR NEW.guided_by IS DISTINCT FROM OLD.guided_by
       OR NEW.occurred_at IS DISTINCT FROM OLD.occurred_at
       OR NEW.sidecar_tables IS DISTINCT FROM OLD.sidecar_tables THEN
        RAISE EXCEPTION 'append-only: % does not accept UPDATE', TG_TABLE_NAME
            USING ERRCODE = '25006';
    END IF;
    IF NEW.blob_id IS DISTINCT FROM OLD.blob_id
       AND NOT EXISTS (
               SELECT 1
                 FROM proxiima_core.blob old_blob
                 JOIN proxiima_core.blob new_blob
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
CREATE OR REPLACE FUNCTION proxiima_core.memory_pin_checks()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
    pin uuid;
    pin_handle uuid;
    pin_targets uuid[] := ARRAY[NEW.t] || NEW.origins || NEW.refs || NEW.goal_refs || NEW.guided_by;
    pin_lock_mode text := current_setting('proxiima_core.pin_lock_mode', true);
    pin_lock_count integer;
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
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
               AND l.relation IN ('proxiima_core.memory'::regclass,
                                  'proxiima_core.goal'::regclass);
        END IF;
        PERFORM set_config('proxiima_core.pin_lock_mode', pin_lock_mode, true);
    END IF;
    -- Take the table locks again instead of trusting the setting: held, this
    -- is a local no-op; a setting bound without them acquires them before any
    -- check below reads.
    IF pin_lock_mode = 'coarse' THEN
        LOCK TABLE proxiima_core.memory, proxiima_core.goal
            IN SHARE ROW EXCLUSIVE MODE;
    ELSE
        PERFORM proxiima_core.lock_pin_targets(pin_targets);
    END IF;

    IF NEW.origins <> '{}' OR NEW.refs <> '{}' THEN
        PERFORM 1
          FROM proxiima_core.memory
         WHERE t = ANY (NEW.origins || NEW.refs)
         ORDER BY t
         FOR SHARE;
    END IF;
    IF NEW.goal_refs <> '{}' THEN
        PERFORM 1
          FROM proxiima_core.goal
         WHERE t = ANY (NEW.goal_refs)
         ORDER BY t
         FOR SHARE;
    END IF;
    IF NEW.guided_by <> '{}' THEN
        PERFORM 1
          FROM proxiima_core.goal
         WHERE t = ANY (NEW.guided_by)
         ORDER BY t
         FOR SHARE;
    END IF;

    IF EXISTS (SELECT 1 FROM proxiima_core.goal WHERE t = NEW.t)
       OR EXISTS (SELECT 1 FROM proxiima_core.erased_pin_target WHERE t = NEW.t)
    THEN
        RAISE EXCEPTION 'memory t % is already a Goal or erased target', NEW.t
            USING ERRCODE = '23505';
    END IF;

    IF NEW.kind = 'fact'
       AND NEW.origins = '{}'
       AND NEW.refs = '{}'
       AND NEW.goal_refs = '{}'
       AND NEW.guided_by = '{}'
    THEN
        PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;

    -- Goal references never provide F/A/P grounding.
    IF NEW.kind <> 'fact'
       AND NOT proxiima_core.pins_have_grounding_support(
             NEW.origins || NEW.refs, NULL, NULL
           )
       AND NOT EXISTS (
           SELECT 1 FROM unnest(NEW.origins || NEW.refs) p(id)
           JOIN proxiima_core.erased_pin_target e ON e.t=p.id AND e.kind<>'goal'
           WHERE proxiima_core.schema_evolution_existing_pin_permits('memory', NEW.t, p.id)
       )
    THEN
        RAISE EXCEPTION 'non-fact must pin a memory'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.origins = '{}' AND NEW.refs = '{}' AND NEW.goal_refs = '{}'
       AND NEW.guided_by = '{}' THEN
        PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;

    -- Ordinary admissions need live origins. Migration may retain a captured
    -- historical origin, whose witness kind is checked below.
    SELECT p.id INTO pin
      FROM unnest(NEW.origins) AS p(id)
      LEFT JOIN proxiima_core.memory m ON m.t = p.id
     WHERE m.t IS NULL
       AND NOT proxiima_core.schema_evolution_existing_pin_permits('memory', NEW.t, p.id)
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'origin pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    -- `refs` carries only Memory targets after the 0004 split.
    SELECT p.id INTO pin
      FROM unnest(NEW.refs) AS p(id)
      LEFT JOIN proxiima_core.memory m ON m.t = p.id
     WHERE m.t IS NULL
       AND NOT proxiima_core.schema_evolution_existing_pin_permits('memory', NEW.t, p.id)
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'reference pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    -- `goal_refs` carries only Goal targets.
    SELECT p.id INTO pin
      FROM unnest(NEW.goal_refs) AS p(id)
      LEFT JOIN proxiima_core.goal g ON g.t = p.id
     WHERE g.t IS NULL
       AND NOT proxiima_core.schema_evolution_existing_pin_permits('memory', NEW.t, p.id)
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'goal reference pin % does not exist as a Goal', pin
            USING ERRCODE = '23503';
    END IF;

    SELECT p.id INTO pin
      FROM unnest(NEW.guided_by) AS p(id)
      LEFT JOIN proxiima_core.goal g ON g.t = p.id
     WHERE g.t IS NULL
       AND NOT proxiima_core.schema_evolution_existing_pin_permits('memory', NEW.t, p.id)
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'guided_by pin % does not exist as a Goal', pin
            USING ERRCODE = '23503';
    END IF;
    IF NEW.guided_by <> '{}' AND NEW.kind <> 'perspective'::proxiima_core.memory_kind THEN
        RAISE EXCEPTION 'only Perspectives may declare guided_by'
            USING ERRCODE = '23514';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM unnest(NEW.guided_by) AS p(id)
          JOIN proxiima_core.goal g ON g.t = p.id
         WHERE g.owner_id <> NEW.owner_id
    ) THEN
        RAISE EXCEPTION 'guided_by target owner differs from Perspective owner'
            USING ERRCODE = '23514';
    END IF;

    SELECT m.handle INTO pin_handle
      FROM proxiima_core.memory m
      JOIN proxiima_core.closed_handle c ON c.handle = m.handle
     WHERE m.t = ANY (NEW.origins || NEW.refs)
       AND NOT proxiima_core.schema_evolution_existing_pin_permits('memory', NEW.t, m.t)
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
                       SELECT 1 FROM proxiima_core.memory m
                        WHERE m.t = o.id AND m.kind IN ('fact', 'abstraction')
                       UNION ALL
                       SELECT 1 FROM proxiima_core.erased_pin_target e
                        WHERE e.t=o.id AND e.kind IN ('fact','abstraction')
                          AND proxiima_core.schema_evolution_existing_pin_permits('memory', NEW.t, o.id)
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
                       SELECT 1 FROM proxiima_core.memory m
                        WHERE m.t = o.id AND m.kind = 'abstraction'
                       UNION ALL
                       SELECT 1 FROM proxiima_core.erased_pin_target e
                        WHERE e.t=o.id AND e.kind='abstraction'
                          AND proxiima_core.schema_evolution_existing_pin_permits('memory', NEW.t, o.id)
                   )
        ) THEN
            RAISE EXCEPTION 'perspective origins must be abstraction t'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$function$;
REVOKE ALL ON FUNCTION proxiima_core.memory_pin_checks() FROM PUBLIC;

-- Owner-RLS functions

CREATE OR REPLACE FUNCTION proxiima_core.owner_group_kind_guard()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
    existing_kind proxiima_core.owner_kind;
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
    -- Personal registration is the v0.0.30 first-seen path.  The owners
    -- primary key and immutable-owner trigger already serialize it; taking a
    -- per-owner advisory lock here would add one lock to every memory write.
    IF NEW.kind = 'personal' THEN
        PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;
    IF NEW.kind = 'group' THEN
        PERFORM proxiima_core.lock_group_tree(NEW.owner_id);
    END IF;
    PERFORM pg_advisory_xact_lock(
        hashtextextended('proxiima-owner-kind:' || NEW.owner_id::text, 0));
    SELECT kind INTO existing_kind
      FROM proxiima_core.owners
     WHERE owner_id = NEW.owner_id;
    IF FOUND THEN
        -- Registration is idempotent.  The owners immutability trigger owns
        -- UPDATE rejection; INSERT ... ON CONFLICT DO NOTHING must be able
        -- to propose either kind without changing an existing registration.
        PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;
    IF NEW.kind = 'group' AND EXISTS (
        SELECT 1 FROM proxiima_core.group_memberships WHERE member_user_id = NEW.owner_id
    ) THEN
        RAISE EXCEPTION 'owner % cannot become a group while it has person memberships', NEW.owner_id;
    END IF;
    IF NEW.kind = 'personal' AND EXISTS (
        SELECT 1 FROM proxiima_core.group_edges WHERE child_group_id = NEW.owner_id
    ) THEN
        RAISE EXCEPTION 'owner % cannot become a person while it is a group member', NEW.owner_id;
    END IF;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.owner_group_kind_guard() FROM PUBLIC;

-- Resolve one declared memory key from its catalog FK, independently of
-- owner mode. Ambiguous keys are refused before generating a policy.
CREATE OR REPLACE FUNCTION proxiima_core.owner_rls_memory_key(caller text, target_schema text, target_table text)
RETURNS text LANGUAGE plpgsql SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    rel_oid oid := format('%I.%I',target_schema,target_table)::regclass;
    memory_key text;
    memory_keys integer;
    memory_on_key boolean;
BEGIN
            SELECT child_att.attname, count(*) OVER (),
                   COALESCE(con.conkey[1] = pk.conkey[1], false)
              INTO memory_key, memory_keys, memory_on_key
              FROM pg_constraint AS con
              JOIN pg_attribute AS child_att ON child_att.attrelid = con.conrelid AND child_att.attnum = con.conkey[1]
              LEFT JOIN pg_constraint AS pk ON pk.conrelid = con.conrelid AND pk.contype = 'p'
             WHERE con.conrelid = rel_oid AND con.contype = 'f'
               AND con.confrelid = 'proxiima_core.memory'::regclass
               AND array_length(con.conkey, 1) = 1
             ORDER BY COALESCE(con.conkey[1] = pk.conkey[1], false) DESC, con.oid
             LIMIT 1;
            IF memory_key IS NOT NULL AND NOT memory_on_key
               AND (memory_keys > 1 OR (memory_key <> 't' AND EXISTS (
                    SELECT 1 FROM pg_attribute
                     WHERE attrelid = rel_oid AND attname = 't' AND attnum > 0 AND NOT attisdropped)))
            THEN
                RAISE EXCEPTION '%: %.% has % foreign key(s) to proxiima_core.memory, none on its leading primary-key column, beside a t column or each other; its memory is ambiguous',
                    caller, target_schema, target_table, memory_keys;
            END IF;
    RETURN COALESCE(memory_key,'t');
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.owner_rls_memory_key(text,text,text) FROM PUBLIC;

-- Generate SQL for an unaliased, schema-qualified target relation.
-- Parent queries retain their ordinary RLS; generation does not bind a scope.
-- FORGET-POLICY: live-only (policy subqueries run under owner RLS) applies to every read in this function
CREATE OR REPLACE FUNCTION proxiima_core.owner_rls_class_predicates(caller text, class_label text, target_schema text, target_table text, class proxiima_core.owner_rls_class, owner_column text)
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
            memory_key := CASE WHEN target_schema = 'proxiima_core' THEN
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
                                SELECT 1 FROM proxiima_core.memory AS parent
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
            IF memory_key IS NULL AND EXISTS (
                SELECT 1 FROM pg_constraint c WHERE c.conrelid=rel_oid AND c.contype='f'
                AND c.confrelid='proxiima_core.memory'::regclass AND array_length(c.conkey,1)=1
            ) THEN
                memory_key := proxiima_core.owner_rls_memory_key(caller,target_schema,target_table);
            END IF;
            IF memory_key IS NOT NULL THEN
                read_expr := format('(%s) OR EXISTS (SELECT 1 FROM proxiima_core.memory parent WHERE parent.t=%I.%I.%I AND parent.owner_id=%I.%I.%I AND proxiima_core.goal_memory_read_admitted(parent.t))',
                    read_expr,target_schema,target_table,memory_key,target_schema,target_table,owner_column);
            END IF;
        WHEN 'memory_owner' THEN
            memory_key := proxiima_core.owner_rls_memory_key(caller,target_schema,target_table);
            IF NOT EXISTS (
                SELECT 1 FROM pg_attribute
                 WHERE attrelid = rel_oid AND attname = memory_key AND atttypid = 'uuid'::regtype
                   AND attnum > 0 AND NOT attisdropped
            ) THEN
                RAISE EXCEPTION '%: %.% is % but has no uuid t column',
                    caller, target_schema, target_table, class_label;
            END IF;
            -- Reading this sidecar follows the parent memory's complete RLS
            -- admission, including exact Goal routes. Do not mirror its read
            -- policy in a second owner/Goal expression here. Writes still
            -- require an owner write ceiling for the parent's actual kind.
            read_expr := format('EXISTS (SELECT 1 FROM proxiima_core.memory AS parent WHERE parent.t = %I.%I.%I)',
                target_schema, target_table, memory_key);
            kind_of := format('CASE parent.kind::text WHEN ''fact'' THEN %s WHEN ''abstraction'' THEN %s ELSE %s END',
                '(SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]',
                '(SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]',
                '(SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]');
            write_expr := format('EXISTS (SELECT 1 FROM proxiima_core.memory AS parent WHERE parent.t = %I.%I.%I AND parent.owner_id = ANY(%s))',
                target_schema, target_table, memory_key, kind_of);
        WHEN 'ownerless' THEN
            read_expr := 'false';
            write_expr := 'false';
        WHEN 'fk_parent' THEN
            fk := proxiima_core.owner_rls_parent_fk(caller, target_schema, target_table);
            fk_child := fk ->> 'child_column';
            fk_schema := fk ->> 'parent_schema';
            fk_table := fk ->> 'parent_table';
            fk_column := fk ->> 'parent_column';
            SELECT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid = format('%I.%I', fk_schema, fk_table)::regclass AND attname = 't' AND attnum > 0 AND NOT attisdropped) INTO parent_has_t;
            read_expr := format('EXISTS (SELECT 1 FROM %I.%I AS parent WHERE parent.%I = %I.%I.%I AND current_setting(''app.proxiima_scope'', true) = ''owner'')', fk_schema, fk_table, fk_column, target_schema, target_table, fk_child);
            IF fk_schema = 'proxiima_core' AND fk_table = 'memory' THEN
                write_expr := format('EXISTS (SELECT 1 FROM proxiima_core.memory AS parent WHERE parent.%I = %I.%I.%I AND ((parent.kind = ''fact'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''abstraction'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''perspective'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))))', fk_column, target_schema, target_table, fk_child);
            ELSIF fk_schema = 'proxiima_core' AND fk_table = 'goal' THEN
                write_expr := format('EXISTS (SELECT 1 FROM proxiima_core.goal AS parent WHERE parent.%I = %I.%I.%I AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_goal'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))', fk_column, target_schema, target_table, fk_child);
            ELSIF parent_has_t THEN
                write_expr := format('EXISTS (SELECT 1 FROM %I.%I AS parent JOIN proxiima_core.memory AS owner_memory ON owner_memory.t = parent.t WHERE parent.%I = %I.%I.%I AND ((owner_memory.kind = ''fact'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (owner_memory.kind = ''abstraction'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (owner_memory.kind = ''perspective'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))))', fk_schema, fk_table, fk_column, target_schema, target_table, fk_child);
            ELSE
                RAISE EXCEPTION 'owner RLS writable classification missing for %.%', target_schema, target_table;
            END IF;
    END CASE;

    select_expr := format('(%s) OR (%s)', read_expr, write_expr);
    RETURN NEXT;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.owner_rls_class_predicates(caller text, class_label text, target_schema text, target_table text, class proxiima_core.owner_rls_class, owner_column text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.owner_rls_apply_class(caller text, class_label text, target_schema text, target_table text, class proxiima_core.owner_rls_class, owner_column text)
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
      FROM proxiima_core.owner_rls_class_predicates(
          caller, class_label, target_schema, target_table, class, owner_column
      ) AS predicates;
    PERFORM proxiima_core.owner_rls_apply_policies(target_schema, target_table, read_expr, write_expr);
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.owner_rls_apply_class(caller text, class_label text, target_schema text, target_table text, class proxiima_core.owner_rls_class, owner_column text) FROM PUBLIC;

-- The three owner-RLS policies of one table from its read and write
-- predicates, with ENABLE and FORCE: the one place they are created.
CREATE OR REPLACE FUNCTION proxiima_core.owner_rls_apply_policies(target_schema text, target_table text, read_expr text, write_expr text)
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
    EXECUTE format('DROP POLICY IF EXISTS proxiima_owner_read ON %I.%I', target_schema, target_table);
    EXECUTE format('DROP POLICY IF EXISTS proxiima_owner_write ON %I.%I', target_schema, target_table);
    EXECUTE format('DROP POLICY IF EXISTS proxiima_platform ON %I.%I', target_schema, target_table);
    EXECUTE format('CREATE POLICY proxiima_owner_read ON %I.%I FOR SELECT TO PUBLIC USING (%s)', target_schema, target_table, read_expr);
    EXECUTE format('CREATE POLICY proxiima_owner_write ON %I.%I FOR ALL TO PUBLIC USING (%s) WITH CHECK (%s)', target_schema, target_table, write_expr, write_expr);
    EXECUTE format('CREATE POLICY proxiima_platform ON %I.%I FOR ALL TO %I USING (current_setting(''app.proxiima_scope'', true) = ''platform'') WITH CHECK (current_setting(''app.proxiima_scope'', true) = ''platform'')', target_schema, target_table, rel_owner);
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.owner_rls_apply_policies(target_schema text, target_table text, read_expr text, write_expr text) FROM PUBLIC;

-- Canonical Goal motivation arrays own the declaration. This scalar projection
-- is platform-maintained solely for selective reverse reads.
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
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.sync_goal_motivation_index() FROM PUBLIC;

-- Goal visibility has an isolated read-only definer authority. It cannot
-- write a table, inherit platform authority, or be assumed by the runtime.
GRANT USAGE, CREATE ON SCHEMA proxiima_core TO proxiima_goal_reader;
GRANT SELECT ON proxiima_core.memory, proxiima_core.memory_head,
    proxiima_core.goal, proxiima_core.goal_head,
    proxiima_core.goal_access_grant, proxiima_core.goal_memory_publication,
    proxiima_core.goal_work_binding TO proxiima_goal_reader;
DO $goal_reader_policies$
DECLARE table_name text;
BEGIN
    FOREACH table_name IN ARRAY ARRAY['memory', 'memory_head', 'goal', 'goal_head',
        'goal_access_grant', 'goal_memory_publication', 'goal_work_binding'] LOOP
        EXECUTE format('DROP POLICY IF EXISTS proxiima_goal_reader ON proxiima_core.%I', table_name);
        EXECUTE format('CREATE POLICY proxiima_goal_reader ON proxiima_core.%I FOR SELECT TO proxiima_goal_reader USING (true)', table_name);
    END LOOP;
END
$goal_reader_policies$;
DO $save_goal_role$ BEGIN PERFORM set_config('app.goal_installer_role', current_user, true); END $save_goal_role$;
SET LOCAL ROLE proxiima_goal_reader;

-- One exact visibility projection, selected through the subject index. The
-- result contains no unpublished current-head identity or ancestor payload.
CREATE OR REPLACE FUNCTION proxiima_core.goal_memory_read_routes()
RETURNS TABLE(memory_t uuid, memory_handle uuid, owner_id uuid, goal_handle uuid, controller_owner uuid)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
    WITH audience_routes AS (
        SELECT a.goal_handle, a.read_ceiling, h.owner_id, h.authority_generation
          FROM proxiima_core.goal_access_grant a
          JOIN proxiima_core.goal_head h ON h.handle = a.goal_handle
           AND h.authority_generation = a.controller_generation
         WHERE current_setting('app.proxiima_scope', true) = 'owner'
           AND a.subject_id = NULLIF(current_setting('app.goal_subject', true), '')::uuid
           AND (a.expires_at IS NULL OR a.expires_at > statement_timestamp())
        UNION ALL
        SELECT h.handle,
               CASE
                   WHEN h.owner_id = ANY(COALESCE(NULLIF(current_setting('app.read_goal', true), '')::uuid[], '{}')) THEN 4
                   WHEN h.owner_id = ANY(COALESCE(NULLIF(current_setting('app.read_perspective', true), '')::uuid[], '{}')) THEN 3
                   WHEN h.owner_id = ANY(COALESCE(NULLIF(current_setting('app.read_abstraction', true), '')::uuid[], '{}')) THEN 2
                   ELSE 1
               END::smallint,
               h.owner_id, h.authority_generation
          FROM proxiima_core.goal_head h
         WHERE current_setting('app.proxiima_scope', true) = 'owner'
           AND NULLIF(current_setting('app.goal_subject', true), '') IS NOT NULL
           AND h.owner_id = ANY(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'))
    ), audience AS (
        SELECT goal_handle, max(read_ceiling) AS read_ceiling, owner_id, authority_generation
          FROM audience_routes
         GROUP BY goal_handle, owner_id, authority_generation
    ), published AS (
        SELECT m.t, m.handle, m.owner_id, a.goal_handle, a.owner_id
          FROM audience a
          JOIN proxiima_core.goal_memory_publication p ON p.goal_handle = a.goal_handle
           AND p.controller_generation = a.authority_generation
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (publications expose live exact admissions under owner RLS) */ ON m.t = p.memory_t AND m.owner_id = p.source_owner
          JOIN proxiima_core.memory_head mh ON mh.handle = m.handle
           AND mh.owner_id = m.owner_id AND mh.authority_generation = p.source_generation
         WHERE m.forgotten_at IS NULL
           AND a.read_ceiling >= CASE m.kind WHEN 'fact' THEN 1 WHEN 'abstraction' THEN 2 WHEN 'perspective' THEN 3 END
    ), working AS (
        SELECT m.t, m.handle, m.owner_id, a.goal_handle, a.owner_id
          FROM audience a
          JOIN proxiima_core.goal_work_binding w ON w.goal_handle = a.goal_handle
           AND w.controller_generation = a.authority_generation AND w.source_owner = a.owner_id
          JOIN proxiima_core.memory_head mh ON mh.handle = w.memory_handle
           AND mh.owner_id = w.source_owner AND mh.authority_generation = w.source_generation
          JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (working routes expose live versions under owner RLS) */ ON m.handle = mh.handle AND m.owner_id = mh.owner_id
         WHERE m.forgotten_at IS NULL
           AND a.read_ceiling >= CASE m.kind WHEN 'fact' THEN 1 WHEN 'abstraction' THEN 2 WHEN 'perspective' THEN 3 END
    )
    SELECT * FROM published UNION SELECT * FROM working
$function$;
REVOKE ALL ON FUNCTION proxiima_core.goal_memory_read_routes() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.goal_visible_memory_ids()
RETURNS TABLE(memory_t uuid, memory_handle uuid, owner_id uuid)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
    SELECT DISTINCT route.memory_t, route.memory_handle, route.owner_id
      FROM proxiima_core.goal_memory_read_routes() route
$function$;
REVOKE ALL ON FUNCTION proxiima_core.goal_visible_memory_ids() FROM PUBLIC;

-- Latest visible Goal publication/work version; an independently readable
-- newer owner head wins. Private heads never replace an audience's version.
-- FORGET-POLICY: live-only (latest visible admissions under owner RLS)
CREATE OR REPLACE FUNCTION proxiima_core.goal_visible_memory_heads()
RETURNS TABLE(memory_t uuid, memory_handle uuid, owner_id uuid)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
    WITH visible AS (
        SELECT DISTINCT ON (v.memory_handle) v.memory_t,v.memory_handle,v.owner_id
          FROM proxiima_core.goal_visible_memory_ids() v
         ORDER BY v.memory_handle,v.memory_t DESC
    )
    SELECT v.memory_t,v.memory_handle,v.owner_id
      FROM visible v
     WHERE NOT EXISTS (
         SELECT 1 FROM proxiima_core.memory_head h
          JOIN proxiima_core.memory m ON m.t=h.t AND m.handle=h.handle
         WHERE h.handle=v.memory_handle AND h.t>v.memory_t AND m.forgotten_at IS NULL
           AND h.owner_id=ANY(CASE h.kind
               WHEN 'fact' THEN COALESCE(NULLIF(current_setting('app.owner',true),'')::uuid[],'{}')
               WHEN 'abstraction' THEN COALESCE(NULLIF(current_setting('app.read_abstraction',true),'')::uuid[],'{}')
               WHEN 'perspective' THEN COALESCE(NULLIF(current_setting('app.read_perspective',true),'')::uuid[],'{}') END)
     )
$function$;
REVOKE ALL ON FUNCTION proxiima_core.goal_visible_memory_heads() FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.goal_memory_read_admitted(candidate_t uuid)
RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
    SELECT EXISTS (SELECT 1 FROM proxiima_core.goal_visible_memory_ids() visible
                    WHERE visible.memory_t = candidate_t)
$function$;
REVOKE ALL ON FUNCTION proxiima_core.goal_memory_read_admitted(uuid) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.goal_read_admitted(candidate_t uuid)
RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path TO 'pg_catalog', 'pg_temp'
AS $function$
    SELECT EXISTS (
        SELECT 1 FROM proxiima_core.goal g
          JOIN proxiima_core.goal_head h ON h.handle = g.handle
          JOIN proxiima_core.goal_access_grant a ON a.goal_handle = h.handle
           AND a.controller_generation = h.authority_generation
         WHERE current_setting('app.proxiima_scope', true) = 'owner'
           AND g.t = candidate_t
           AND a.subject_id = NULLIF(current_setting('app.goal_subject', true), '')::uuid
           AND (a.expires_at IS NULL OR a.expires_at > statement_timestamp())
           AND a.read_ceiling >= 4
    )
$function$;
REVOKE ALL ON FUNCTION proxiima_core.goal_read_admitted(uuid) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION proxiima_core.goal_memory_read_routes(),
    proxiima_core.goal_visible_memory_ids(), proxiima_core.goal_visible_memory_heads(),
    proxiima_core.goal_memory_read_admitted(uuid),
    proxiima_core.goal_read_admitted(uuid) TO proxiima_search;
-- The role's database-local default privileges supply the runtime EXECUTE
-- grant, as for proxiima_search. No platform function becomes callable.
DO $grant_goal_installer$
BEGIN
    EXECUTE format('GRANT EXECUTE ON FUNCTION proxiima_core.goal_memory_read_routes(), proxiima_core.goal_visible_memory_ids(), proxiima_core.goal_visible_memory_heads(), proxiima_core.goal_memory_read_admitted(uuid), proxiima_core.goal_read_admitted(uuid) TO %I', current_setting('app.goal_installer_role'));
END
$grant_goal_installer$;
DO $restore_goal_role$ BEGIN EXECUTE format('SET LOCAL ROLE %I', current_setting('app.goal_installer_role')); END $restore_goal_role$;
REVOKE CREATE ON SCHEMA proxiima_core FROM proxiima_goal_reader;

-- The host's follows rule for a pg_sidecar!(owner: follows) table: the
-- owner_id class predicate AND the memory_owner class predicate. The
-- owner predicate includes current-owner-aligned exact Goal publication reads. The memory half reads through the row's memory under that
-- memory's own policies, so a forgotten memory hides the row from owner
-- scopes and the lifecycle scope still reaches it; the owner_id half lets the
-- planner use an owner_id index. Follows keeps owner_id equal to the
-- memory's owner, so the AND admits exactly the rows memory_owner admits.
CREATE OR REPLACE FUNCTION proxiima_core.owner_rls_follows_predicates(caller text, target_schema text, target_table text)
 RETURNS TABLE(read_expr text, write_expr text)
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    by_owner record;
    by_memory record;
BEGIN
    SELECT * INTO by_owner
      FROM proxiima_core.owner_rls_class_predicates(
          caller, 'a Follows table', target_schema, target_table, 'owner_id', 'owner_id');
    SELECT * INTO by_memory
      FROM proxiima_core.owner_rls_class_predicates(
          caller, 'a Follows table', target_schema, target_table, 'memory_owner', 'owner_id');
    read_expr := format('(%s) AND (%s)', by_owner.read_expr, by_memory.read_expr);
    write_expr := format('(%s) AND (%s)', by_owner.write_expr, by_memory.write_expr);
    RETURN NEXT;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.owner_rls_follows_predicates(caller text, target_schema text, target_table text) FROM PUBLIC;

-- Install the follows rule on one existing spiice table. The table must
-- carry a uuid owner_id column: freeze requires it of every Follows sidecar.
CREATE OR REPLACE FUNCTION proxiima_core.owner_rls_apply_follows(caller text, target_schema text, target_table text)
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
      FROM proxiima_core.owner_rls_follows_predicates(caller, target_schema, target_table) AS predicates;
    PERFORM proxiima_core.owner_rls_apply_policies(target_schema, target_table, read_expr, write_expr);
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.owner_rls_apply_follows(caller text, target_schema text, target_table text) FROM PUBLIC;

-- Candidate policies are limited to derived projections, the local vector index
-- and the core sidecars the substring candidate function reads through their
-- trigram indexes.
CREATE OR REPLACE FUNCTION proxiima_core.is_search_candidate_relation(relation_id oid)
 RETURNS boolean
 LANGUAGE sql STABLE
 SET search_path TO 'pg_catalog'
AS $function$
    SELECT EXISTS (
        SELECT 1 FROM pg_class relation JOIN pg_namespace namespace ON namespace.oid = relation.relnamespace
         WHERE relation.oid = relation_id AND relation.relkind IN ('r', 'p')
           AND ((namespace.nspname = 'proxiima_core' AND relation.relname IN (
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
                    AND fk.confrelid = 'proxiima_core.memory'::regclass
                    AND fk.conkey = ARRAY[child.attnum] AND fk.confkey = ARRAY[parent.attnum]
                    AND child.atttypid = 'uuid'::regtype AND NOT child.attisdropped
             )))
    )
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.owner_rls_census_table(target_schema text, target_table text)
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
     WHERE polrelid = relation.oid AND polname = 'proxiima_search_candidates';
    IF FOUND THEN
        IF NOT proxiima_core.is_search_candidate_relation(relation.oid)
           OR candidate_policy.polcmd <> 'r' OR NOT candidate_policy.polpermissive
           OR candidate_policy.polroles IS DISTINCT FROM ARRAY[to_regrole('proxiima_search')::oid]
           OR pg_get_expr(candidate_policy.polqual, relation.oid) IS DISTINCT FROM 'true'
           OR candidate_policy.polwithcheck IS NOT NULL
        THEN
            RAISE EXCEPTION 'table %.% has an invalid search candidate policy', relation.schema_name, relation.relname;
        END IF;
        expected_policies := expected_policies + 1;
    END IF;
    SELECT * INTO candidate_policy FROM pg_policy
     WHERE polrelid = relation.oid AND polname = 'proxiima_goal_reader';
    IF FOUND THEN
        IF relation.schema_name <> 'proxiima_core'
           OR relation.relname <> ALL(ARRAY['memory', 'memory_head', 'goal', 'goal_head',
               'goal_access_grant', 'goal_memory_publication', 'goal_work_binding'])
           OR candidate_policy.polcmd <> 'r' OR NOT candidate_policy.polpermissive
           OR candidate_policy.polroles IS DISTINCT FROM ARRAY[to_regrole('proxiima_goal_reader')::oid]
           OR pg_get_expr(candidate_policy.polqual, relation.oid) IS DISTINCT FROM 'true'
           OR candidate_policy.polwithcheck IS NOT NULL
        THEN
            RAISE EXCEPTION 'table %.% has an invalid Goal reader policy', relation.schema_name, relation.relname;
        END IF;
        expected_policies := expected_policies + 1;
    END IF;
    IF (SELECT count(*) FROM pg_policy WHERE polrelid = relation.oid) <> expected_policies THEN
        IF expected_policies = 3 THEN
            RAISE EXCEPTION 'table %.% does not have exactly three RLS policies', relation.schema_name, relation.relname;
        ELSE
            RAISE EXCEPTION 'table %.% has unexpected RLS policies', relation.schema_name, relation.relname;
        END IF;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_policy WHERE polrelid = relation.oid AND polname = 'proxiima_platform' AND polroles = ARRAY[relation.relowner] AND polcmd = '*') THEN
        RAISE EXCEPTION 'table %.% platform policy is not owner-role limited', relation.schema_name, relation.relname;
    END IF;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.owner_rls_census_table(target_schema text, target_table text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.owner_rls_find_parent_fk(target_schema text, target_table text)
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
       AND parent_ns.nspname IN ('proxiima_core', target_schema)
       -- An FK to a partitioned parent adds one row per referenced
       -- partition, each a clone of this table's own parent-level FK;
       -- one a partition inherits from its partitioned table stays.
       AND NOT EXISTS (SELECT 1 FROM pg_constraint AS clone_of
                        WHERE clone_of.oid = con.conparentid AND clone_of.conrelid = con.conrelid)
     ORDER BY COALESCE(con.conkey[1] = pk.conkey[1], false) DESC,
              (parent_ns.nspname = 'proxiima_core' AND parent.relname IN ('memory', 'goal')) DESC,
              con.oid
     LIMIT 1;
    IF fk IS NULL THEN
        RETURN NULL;
    END IF;
    RETURN to_jsonb(fk);
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.owner_rls_find_parent_fk(target_schema text, target_table text) FROM PUBLIC;

CREATE OR REPLACE FUNCTION proxiima_core.owner_rls_parent_fk(caller text, target_schema text, target_table text)
 RETURNS jsonb
 LANGUAGE plpgsql
 SET search_path TO 'pg_catalog'
AS $function$
DECLARE
    fk jsonb := proxiima_core.owner_rls_find_parent_fk(target_schema, target_table);
BEGIN
    IF fk IS NULL THEN
        RAISE EXCEPTION 'owner RLS classification missing for %.%: no single-column FK to proxiima_core or %',
            target_schema, target_table, target_schema;
    END IF;
    IF NOT (fk ->> 'on_key')::boolean AND (fk ->> 'candidates')::integer > 1 THEN
        RAISE EXCEPTION '%: %.% has % candidate parent FKs and none on its leading primary-key column; its owner is ambiguous',
            caller, target_schema, target_table, fk ->> 'candidates';
    END IF;
    RETURN fk;
END
$function$;
REVOKE ALL ON FUNCTION proxiima_core.owner_rls_parent_fk(caller text, target_schema text, target_table text) FROM PUBLIC;

-- Pin functions

-- FORGET-POLICY: includes-forgotten (grounding support counts forgotten sources) applies to every read in this function
CREATE OR REPLACE FUNCTION proxiima_core.pins_have_grounding_support(pins uuid[], cooling uuid, cooling_kind proxiima_core.memory_kind)
 RETURNS boolean
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxiima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxiima_scope', true);
    supported boolean;
BEGIN
    PERFORM set_config('app.proxiima_scope', 'platform', true);
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
                        SELECT 1 FROM proxiima_core.memory h
                         WHERE h.t = p.id
                   )
               END
    ) INTO supported;
    PERFORM set_config('app.proxiima_scope', COALESCE(previous_scope, ''), true);
    RETURN supported;
END;
$function$;
REVOKE ALL ON FUNCTION proxiima_core.pins_have_grounding_support(pins uuid[], cooling uuid, cooling_kind proxiima_core.memory_kind) FROM PUBLIC;

-- Store identity functions

CREATE OR REPLACE FUNCTION proxiima_core.enforce_store_identity_immutable()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'write-once: the store number cannot be changed or removed'
        USING ERRCODE = '25006';
END;
$$;

CREATE OR REPLACE FUNCTION proxiima_core.memory_created_at(id uuid)
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
CREATE OR REPLACE FUNCTION proxiima_core.publication_counter_forward_only() RETURNS trigger
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

CREATE OR REPLACE AGGREGATE proxiima_core.tsquery_or_agg(tsquery) (
    SFUNC = tsquery_or,
    STYPE = tsquery,
    COMBINEFUNC = tsquery_or,
    PARALLEL = safe
);

-- Memory read/write policies both require their base owner set before kind checks.
-- Candidates use that union; ordinary memory RLS remains the per-kind authority.
CREATE OR REPLACE FUNCTION proxiima_core.search_owner_ids()
 RETURNS uuid[]
 LANGUAGE sql STABLE
 SET search_path TO 'pg_catalog'
AS $function$
    SELECT CASE WHEN current_setting('app.proxiima_scope', true) = 'owner'
           THEN ARRAY(SELECT DISTINCT owner_id
                        FROM unnest(
                            COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}')
                         || COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}')
                        ) AS scoped(owner_id))
           ELSE '{}'::uuid[] END
$function$;

CREATE OR REPLACE FUNCTION proxiima_core.assert_search_scope(requested_owners uuid[])
 RETURNS void
 LANGUAGE plpgsql STABLE
 SET search_path TO 'pg_catalog'
AS $function$
BEGIN
    IF current_setting('app.proxiima_scope', true) IS DISTINCT FROM 'owner'
       OR NULLIF(current_setting('app.owner', true), '') IS NULL
       OR requested_owners IS NULL
       OR array_position(requested_owners, NULL) IS NOT NULL
       OR NOT requested_owners <@ (proxiima_core.search_owner_ids()
           || ARRAY(SELECT DISTINCT visible.owner_id FROM proxiima_core.goal_visible_memory_ids() visible))
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
GRANT USAGE, CREATE ON SCHEMA proxiima_core TO proxiima_search;
GRANT SELECT ON proxiima_core.agent_derivation_v1, proxiima_core.agent_note_v1, proxiima_core.interpretation_v1, proxiima_core.lexical_default, proxiima_core.lexical_languages, proxiima_core.memory, proxiima_core.memory_head, proxiima_core.projection, proxiima_core.utterance_v1 TO proxiima_search;
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.projection;
CREATE POLICY proxiima_search_candidates ON proxiima_core.projection FOR SELECT TO proxiima_search USING (true);
SET LOCAL ROLE proxiima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxiima_core.search_lexical_candidates(query_text text, candidate_limit bigint, requested_tags text[], since_time timestamptz, until_time timestamptz, before_id uuid, requested_owners uuid[], requested_schemas text[], rescue boolean, recency boolean, heads_only boolean, requested_kind text, tag_all boolean)
RETURNS TABLE(t uuid, schema_id text, lexical_score real)
LANGUAGE plpgsql SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
AS $candidate_function$
BEGIN
PERFORM proxiima_core.assert_search_scope(requested_owners);
IF candidate_limit IS NULL OR candidate_limit < 1 OR candidate_limit > 1000 THEN
RAISE EXCEPTION 'candidate limit must be between 1 and 1000' USING ERRCODE = '22023';
END IF;
RETURN QUERY EXECUTE 'WITH analyzed_query AS MATERIALIZED (
             SELECT proxiima_core.lexical_scrub($1) AS scrubbed,
                    proxiima_core.lexical_query_text(proxiima_core.lexical_config(),
                        proxiima_core.lexical_scrub($1)) AS analyzed
         ), q AS MATERIALIZED (
             SELECT s.scrubbed, s.analyzed,
                    COALESCE(
                        (SELECT proxiima_core.tsquery_or_agg(
                                    websearch_to_tsquery(l.config, s.analyzed)
                                    ORDER BY l.config)
                           FROM proxiima_core.lexical_languages l),
                        websearch_to_tsquery(proxiima_core.lexical_config(), s.analyzed)
                    ) AS tsq,
                    COALESCE(
                        (SELECT proxiima_core.tsquery_or_agg(
                                    NULLIF(
                                        replace(
                                            plainto_tsquery(l.config, s.analyzed)::text,
                                            '' & '', '' | ''),
                                        '''')::tsquery
                                    ORDER BY l.config)
                           FROM proxiima_core.lexical_languages l),
                        NULLIF(
                            replace(
                                plainto_tsquery(proxiima_core.lexical_config(), s.analyzed)::text,
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
           FROM proxiima_core.projection p, q
          WHERE p.owner_id = ANY($7::uuid[])
            AND p.schema_id = ANY($8::text[])
            AND (p.search_tsv @@ q.tsq OR ($9::boolean AND q.any_tsq IS NOT NULL AND p.search_tsv @@ q.any_tsq))
             AND (p.owner_id = ANY(proxiima_core.search_owner_ids())
                   OR p.memory_id IN (SELECT memory_t FROM proxiima_core.goal_visible_memory_ids() WHERE owner_id=ANY($7::uuid[]))) AND ($3::text[] IS NULL OR CASE WHEN $12::boolean THEN p.tag @> $3::text[] ELSE p.tag && $3::text[] END) AND EXISTS (SELECT 1 FROM proxiima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = p.memory_id AND (authorized_memory.owner_id = ANY($7::uuid[]) OR authorized_memory.t IN (SELECT memory_t FROM proxiima_core.goal_visible_memory_ids() WHERE owner_id=ANY($7::uuid[]))) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR authorized_memory.t IN (SELECT memory_t FROM proxiima_core.goal_visible_memory_heads()) OR EXISTS (SELECT 1 FROM proxiima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t)))
            AND ($4::timestamptz IS NULL
                 OR proxiima_core.memory_created_at(p.memory_id) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxiima_core.memory_created_at(p.memory_id) <= $5)
            AND ($6::uuid IS NULL OR p.memory_id < $6)' || CASE WHEN recency THEN ' ORDER BY p.memory_id DESC' ELSE ' ORDER BY lexical_score DESC, p.memory_id DESC' END || ' LIMIT $2' USING query_text, candidate_limit, requested_tags, since_time, until_time, before_id, requested_owners, requested_schemas, rescue, heads_only, requested_kind, tag_all;
END
$candidate_function$;$generated_function$;
REVOKE ALL ON FUNCTION proxiima_core.search_lexical_candidates(text, bigint, text[], timestamp with time zone, timestamp with time zone, uuid, uuid[], text[], boolean, boolean, boolean, text, boolean) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxiima_core FROM proxiima_search;
END
$install_candidate$;

DO $install_candidate$
DECLARE original_role name := current_user;
BEGIN
GRANT USAGE, CREATE ON SCHEMA proxiima_core TO proxiima_search;
GRANT SELECT ON proxiima_core.agent_derivation_v1, proxiima_core.agent_note_v1, proxiima_core.interpretation_v1, proxiima_core.lexical_default, proxiima_core.lexical_languages, proxiima_core.memory, proxiima_core.memory_head, proxiima_core.projection, proxiima_core.utterance_v1 TO proxiima_search;
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.projection;
CREATE POLICY proxiima_search_candidates ON proxiima_core.projection FOR SELECT TO proxiima_search USING (true);
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.agent_derivation_v1;
CREATE POLICY proxiima_search_candidates ON proxiima_core.agent_derivation_v1 FOR SELECT TO proxiima_search USING (true);
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.agent_note_v1;
CREATE POLICY proxiima_search_candidates ON proxiima_core.agent_note_v1 FOR SELECT TO proxiima_search USING (true);
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.interpretation_v1;
CREATE POLICY proxiima_search_candidates ON proxiima_core.interpretation_v1 FOR SELECT TO proxiima_search USING (true);
DROP POLICY IF EXISTS proxiima_search_candidates ON proxiima_core.utterance_v1;
CREATE POLICY proxiima_search_candidates ON proxiima_core.utterance_v1 FOR SELECT TO proxiima_search USING (true);
SET LOCAL ROLE proxiima_search;
EXECUTE $generated_function$CREATE OR REPLACE FUNCTION proxiima_core.search_substring_candidates(query_text text, candidate_limit bigint, requested_tags text[], since_time timestamptz, until_time timestamptz, before_id uuid, requested_owners uuid[], requested_schemas text[], rescue boolean, recency boolean, heads_only boolean, requested_kind text, tag_all boolean)
RETURNS TABLE(t uuid, lexical_score real)
LANGUAGE plpgsql SECURITY DEFINER
SET search_path TO pg_catalog, pg_temp
AS $candidate_function$
BEGIN
PERFORM proxiima_core.assert_search_scope(requested_owners);
IF candidate_limit IS NULL OR candidate_limit < 1 OR candidate_limit > 1000 THEN
RAISE EXCEPTION 'candidate limit must be between 1 and 1000' USING ERRCODE = '22023';
END IF;
DECLARE legs text[] := ARRAY[]::text[];
BEGIN
IF requested_schemas && ARRAY['core/agent-derivation-v1']::text[] THEN legs := legs || '(SELECT * FROM (
         WITH eligible_memory AS MATERIALIZED (
             SELECT source.t, source.handle, source.kind, source.owner_id, source.schema_id
               FROM proxiima_core.memory source /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
              WHERE source.owner_id = ANY($7::uuid[])
                AND source.schema_id = ANY($8::text[])
                AND source.schema_id = ANY(ARRAY[''core/agent-derivation-v1'']::text[])
         )
         SELECT m.t AS t,
                0.25::real AS lexical_score
           FROM eligible_memory m
           JOIN proxiima_core.agent_derivation_v1 c
             ON c.t = m.t
            AND (lower(proxiima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.title, ''''), NULLIF(c.body, ''''), proxiima_core.lexical_text_array(c.tags)])) LIKE $1 ESCAPE ''\'')
             AND ($3::text[] IS NULL OR CASE WHEN $12::boolean THEN c.tags @> $3::text[] ELSE c.tags && $3::text[] END)
          WHERE EXISTS (SELECT 1 FROM eligible_memory)
             AND EXISTS (SELECT 1 FROM proxiima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND (authorized_memory.owner_id = ANY($7::uuid[]) OR authorized_memory.t IN (SELECT memory_t FROM proxiima_core.goal_visible_memory_ids() WHERE owner_id=ANY($7::uuid[]))) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR authorized_memory.t IN (SELECT memory_t FROM proxiima_core.goal_visible_memory_heads()) OR EXISTS (SELECT 1 FROM proxiima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/agent-derivation-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxiima_core.memory_created_at(m.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxiima_core.memory_created_at(m.t) <= $5)
            AND ($6::uuid IS NULL OR m.t < $6)
         ) leg)'::text; END IF;
IF requested_schemas && ARRAY['core/agent-note-v1']::text[] THEN legs := legs || '(SELECT * FROM (
         WITH eligible_memory AS MATERIALIZED (
             SELECT source.t, source.handle, source.kind, source.owner_id, source.schema_id
               FROM proxiima_core.memory source /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
              WHERE source.owner_id = ANY($7::uuid[])
                AND source.schema_id = ANY($8::text[])
                AND source.schema_id = ANY(ARRAY[''core/agent-note-v1'']::text[])
         )
         SELECT m.t AS t,
                0.25::real AS lexical_score
           FROM eligible_memory m
           JOIN proxiima_core.agent_note_v1 c
             ON c.t = m.t
            AND (lower(proxiima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.title, ''''), NULLIF(c.body, ''''), proxiima_core.lexical_text_array(c.tags)])) LIKE $1 ESCAPE ''\'')
             AND ($3::text[] IS NULL OR CASE WHEN $12::boolean THEN c.tags @> $3::text[] ELSE c.tags && $3::text[] END)
          WHERE EXISTS (SELECT 1 FROM eligible_memory)
             AND EXISTS (SELECT 1 FROM proxiima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND (authorized_memory.owner_id = ANY($7::uuid[]) OR authorized_memory.t IN (SELECT memory_t FROM proxiima_core.goal_visible_memory_ids() WHERE owner_id=ANY($7::uuid[]))) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR authorized_memory.t IN (SELECT memory_t FROM proxiima_core.goal_visible_memory_heads()) OR EXISTS (SELECT 1 FROM proxiima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/agent-note-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxiima_core.memory_created_at(m.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxiima_core.memory_created_at(m.t) <= $5)
            AND ($6::uuid IS NULL OR m.t < $6)
         ) leg)'::text; END IF;
IF requested_schemas && ARRAY['core/interpretation-v1']::text[] THEN legs := legs || '(SELECT * FROM (
         WITH eligible_memory AS MATERIALIZED (
             SELECT source.t, source.handle, source.kind, source.owner_id, source.schema_id
               FROM proxiima_core.memory source /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
              WHERE source.owner_id = ANY($7::uuid[])
                AND source.schema_id = ANY($8::text[])
                AND source.schema_id = ANY(ARRAY[''core/interpretation-v1'']::text[])
         )
         SELECT m.t AS t,
                0.25::real AS lexical_score
           FROM eligible_memory m
           JOIN proxiima_core.interpretation_v1 c
             ON c.t = m.t
            AND (lower(proxiima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.claim, '''')])) LIKE $1 ESCAPE ''\'')
             AND $3::text[] IS NULL
          WHERE EXISTS (SELECT 1 FROM eligible_memory)
             AND EXISTS (SELECT 1 FROM proxiima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND (authorized_memory.owner_id = ANY($7::uuid[]) OR authorized_memory.t IN (SELECT memory_t FROM proxiima_core.goal_visible_memory_ids() WHERE owner_id=ANY($7::uuid[]))) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR authorized_memory.t IN (SELECT memory_t FROM proxiima_core.goal_visible_memory_heads()) OR EXISTS (SELECT 1 FROM proxiima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/interpretation-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxiima_core.memory_created_at(m.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxiima_core.memory_created_at(m.t) <= $5)
            AND ($6::uuid IS NULL OR m.t < $6)
         ) leg)'::text; END IF;
IF requested_schemas && ARRAY['core/utterance-v1']::text[] THEN legs := legs || '(SELECT * FROM (
         WITH eligible_memory AS MATERIALIZED (
             SELECT source.t, source.handle, source.kind, source.owner_id, source.schema_id
               FROM proxiima_core.memory source /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */
              WHERE source.owner_id = ANY($7::uuid[])
                AND source.schema_id = ANY($8::text[])
                AND source.schema_id = ANY(ARRAY[''core/utterance-v1'']::text[])
         )
         SELECT m.t AS t,
                0.25::real AS lexical_score
           FROM eligible_memory m
           JOIN proxiima_core.utterance_v1 c
             ON c.t = m.t
            AND (lower(proxiima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.text, '''')])) LIKE $1 ESCAPE ''\'')
             AND $3::text[] IS NULL
          WHERE EXISTS (SELECT 1 FROM eligible_memory)
             AND EXISTS (SELECT 1 FROM proxiima_core.memory authorized_memory /* FORGET-POLICY: live-only (candidate authorization under owner RLS) */ WHERE authorized_memory.t = m.t AND (authorized_memory.owner_id = ANY($7::uuid[]) OR authorized_memory.t IN (SELECT memory_t FROM proxiima_core.goal_visible_memory_ids() WHERE owner_id=ANY($7::uuid[]))) AND authorized_memory.forgotten_at IS NULL AND ($11::text IS NULL OR authorized_memory.kind::text = $11) AND (NOT $10::boolean OR authorized_memory.t IN (SELECT memory_t FROM proxiima_core.goal_visible_memory_heads()) OR EXISTS (SELECT 1 FROM proxiima_core.memory_head authorized_head WHERE authorized_head.handle = authorized_memory.handle AND authorized_head.t = authorized_memory.t AND authorized_head.schema_id = ANY(ARRAY[''core/utterance-v1'']::text[]))))
            AND ($4::timestamptz IS NULL
                 OR proxiima_core.memory_created_at(m.t) >= $4)
            AND ($5::timestamptz IS NULL
                 OR proxiima_core.memory_created_at(m.t) <= $5)
            AND ($6::uuid IS NULL OR m.t < $6)
         ) leg)'::text; END IF;
IF cardinality(legs) = 0 THEN RETURN; END IF;
RETURN QUERY EXECUTE 'SELECT s.t, s.lexical_score FROM (' || array_to_string(legs, ' UNION ALL ') || ') s' || CASE WHEN recency THEN ' ORDER BY s.t DESC' ELSE ' ORDER BY s.lexical_score DESC, s.t DESC' END || ' LIMIT $2' USING query_text, candidate_limit, requested_tags, since_time, until_time, before_id, requested_owners, requested_schemas, rescue, heads_only, requested_kind, tag_all;
END;
END
$candidate_function$;$generated_function$;
REVOKE ALL ON FUNCTION proxiima_core.search_substring_candidates(text, bigint, text[], timestamp with time zone, timestamp with time zone, uuid, uuid[], text[], boolean, boolean, boolean, text, boolean) FROM PUBLIC;
EXECUTE format('SET LOCAL ROLE %I', original_role);
REVOKE CREATE ON SCHEMA proxiima_core FROM proxiima_search;
END
$install_candidate$;
