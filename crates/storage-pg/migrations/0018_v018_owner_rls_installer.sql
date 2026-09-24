-- v0.0.18: the owner-RLS installer every flavor schema calls.
--
-- Each flavor's v0.0.15 cutover carried its own copy of the same DO block,
-- differing only in the schema name and the table classification, with core's
-- GUC vocabulary (`app.owner`, `app.write_*`, `app.proxima_scope`) inlined in
-- every copy. This is that block once, parameterised by the classification.
--
-- Classification (bare table names in `target_schema`):
--   owner_id_tables    carry their own `owner_id`: read `app.owner`, write
--                      `app.write_owner`.
--   fk_parent_tables   reach their owner through a single-column FK to a
--                      proxima_core table or a table in `target_schema`: read
--                      is the parent's own RLS in owner scope; write follows
--                      the parent memory's kind (`app.write_owner`,
--                      `app.write_abstraction`, `app.write_perspective`), or
--                      `app.write_goal` for a goal parent.
--   ownerless_tables   platform-only: owner scope reads and writes nothing.
--   memory_owner_tables  keyed by a proxima_core.memory `t`, owner checked on
--                      that row directly: read `app.owner`, write
--                      `app.write_owner` whatever the memory's kind. Optional:
--                      the code flavor's two Self sidecars take this in place
--                      of the FK-parent policy (v0.0.15), which the other three
--                      lists cannot express.
-- Every base table in the schema is in exactly one list, a table with an
-- `owner_id` column is in owner_id_tables, and every listed name exists;
-- anything else raises before a policy changes. Each table then gets ENABLE +
-- FORCE RLS and exactly `proxima_owner_read`, `proxima_owner_write` and
-- `proxima_platform` (the table owner in platform scope), which the runtime
-- RLS guard checks at boot.
--
-- SECURITY INVOKER and EXECUTE for its owner only: it runs the DDL as the
-- migration role, which must own the tables. Re-running it re-creates the
-- same policies, so a later migration that adds a table calls it again with
-- the full classification.

CREATE FUNCTION proxima_core.install_owner_rls(
    target_schema text,
    owner_id_tables text[],
    fk_parent_tables text[],
    ownerless_tables text[],
    memory_owner_tables text[] DEFAULT '{}'
)
RETURNS void
LANGUAGE plpgsql
VOLATILE
SECURITY INVOKER
SET search_path = pg_catalog
AS $install_owner_rls$
DECLARE
    classified text[] := COALESCE(owner_id_tables, '{}') || COALESCE(fk_parent_tables, '{}')
        || COALESCE(ownerless_tables, '{}') || COALESCE(memory_owner_tables, '{}');
    listed text;
    relation record;
    fk record;
    parent_has_t boolean;
    read_expr text;
    write_expr text;
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

    FOR relation IN
        SELECT c.oid, n.nspname AS schema_name, c.relname, c.relowner,
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
        IF relation.has_owner_id AND relation.relname <> ALL(COALESCE(owner_id_tables, '{}')) THEN
            RAISE EXCEPTION 'install_owner_rls: %.% has an owner_id column but is not in owner_id_tables',
                relation.schema_name, relation.relname;
        END IF;

        IF relation.relname = ANY(COALESCE(owner_id_tables, '{}')) THEN
            IF NOT relation.has_owner_id THEN
                RAISE EXCEPTION 'install_owner_rls: %.% is in owner_id_tables but has no owner_id column',
                    relation.schema_name, relation.relname;
            END IF;
            read_expr := $expr$owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
            write_expr := $expr$owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
        ELSIF relation.relname = ANY(COALESCE(memory_owner_tables, '{}')) THEN
            IF NOT EXISTS (
                SELECT 1 FROM pg_attribute
                 WHERE attrelid = relation.oid AND attname = 't' AND atttypid = 'uuid'::regtype
                   AND attnum > 0 AND NOT attisdropped
            ) THEN
                RAISE EXCEPTION 'install_owner_rls: %.% is in memory_owner_tables but has no uuid t column',
                    relation.schema_name, relation.relname;
            END IF;
            read_expr := format('EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = %I.%I.t AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))', relation.schema_name, relation.relname);
            write_expr := format('EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = %I.%I.t AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))', relation.schema_name, relation.relname);
        ELSIF relation.relname = ANY(COALESCE(ownerless_tables, '{}')) THEN
            read_expr := 'false';
            write_expr := 'false';
        ELSE
            SELECT child_att.attname AS child_column,
                   parent_ns.nspname AS parent_schema,
                   parent.relname AS parent_table,
                   parent_att.attname AS parent_column
              INTO fk
              FROM pg_constraint AS con
              JOIN pg_attribute AS child_att ON child_att.attrelid = relation.oid AND child_att.attnum = con.conkey[1]
              JOIN pg_class AS parent ON parent.oid = con.confrelid
              JOIN pg_namespace AS parent_ns ON parent_ns.oid = parent.relnamespace
              JOIN pg_attribute AS parent_att ON parent_att.attrelid = parent.oid AND parent_att.attnum = con.confkey[1]
             WHERE con.conrelid = relation.oid AND con.contype = 'f'
               AND array_length(con.conkey, 1) = 1 AND array_length(con.confkey, 1) = 1
               AND parent_ns.nspname IN ('proxima_core', target_schema)
             ORDER BY (parent_ns.nspname = 'proxima_core' AND parent.relname IN ('memory', 'goal')) DESC, con.oid
             LIMIT 1;
            IF fk IS NULL THEN
                RAISE EXCEPTION 'owner RLS classification missing for %.%: no single-column FK to proxima_core or %',
                    relation.schema_name, relation.relname, target_schema;
            END IF;
            SELECT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid = format('%I.%I', fk.parent_schema, fk.parent_table)::regclass AND attname = 't' AND attnum > 0 AND NOT attisdropped) INTO parent_has_t;
            read_expr := format('EXISTS (SELECT 1 FROM %I.%I AS parent WHERE parent.%I = %I.%I.%I AND current_setting(''app.proxima_scope'', true) = ''owner'')', fk.parent_schema, fk.parent_table, fk.parent_column, relation.schema_name, relation.relname, fk.child_column);
            IF fk.parent_schema = 'proxima_core' AND fk.parent_table = 'memory' THEN
                write_expr := format('EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.%I = %I.%I.%I AND ((parent.kind = ''fact'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''abstraction'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''perspective'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))))', fk.parent_column, relation.schema_name, relation.relname, fk.child_column);
            ELSIF fk.parent_schema = 'proxima_core' AND fk.parent_table = 'goal' THEN
                write_expr := format('EXISTS (SELECT 1 FROM proxima_core.goal AS parent WHERE parent.%I = %I.%I.%I AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_goal'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))', fk.parent_column, relation.schema_name, relation.relname, fk.child_column);
            ELSIF parent_has_t THEN
                write_expr := format('EXISTS (SELECT 1 FROM %I.%I AS parent JOIN proxima_core.memory AS owner_memory ON owner_memory.t = parent.t WHERE parent.%I = %I.%I.%I AND ((owner_memory.kind = ''fact'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (owner_memory.kind = ''abstraction'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (owner_memory.kind = ''perspective'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))))', fk.parent_schema, fk.parent_table, fk.parent_column, relation.schema_name, relation.relname, fk.child_column);
            ELSE
                RAISE EXCEPTION 'owner RLS writable classification missing for %.%', relation.schema_name, relation.relname;
            END IF;
        END IF;

        EXECUTE format('ALTER TABLE %I.%I ENABLE ROW LEVEL SECURITY', relation.schema_name, relation.relname);
        EXECUTE format('ALTER TABLE %I.%I FORCE ROW LEVEL SECURITY', relation.schema_name, relation.relname);
        EXECUTE format('DROP POLICY IF EXISTS proxima_owner_read ON %I.%I', relation.schema_name, relation.relname);
        EXECUTE format('DROP POLICY IF EXISTS proxima_owner_write ON %I.%I', relation.schema_name, relation.relname);
        EXECUTE format('DROP POLICY IF EXISTS proxima_platform ON %I.%I', relation.schema_name, relation.relname);
        EXECUTE format('CREATE POLICY proxima_owner_read ON %I.%I FOR SELECT TO PUBLIC USING (%s)', relation.schema_name, relation.relname, read_expr);
        EXECUTE format('CREATE POLICY proxima_owner_write ON %I.%I FOR ALL TO PUBLIC USING (%s) WITH CHECK (%s)', relation.schema_name, relation.relname, write_expr, write_expr);
        EXECUTE format('CREATE POLICY proxima_platform ON %I.%I FOR ALL TO %I USING (current_setting(''app.proxima_scope'', true) = ''platform'') WITH CHECK (current_setting(''app.proxima_scope'', true) = ''platform'')', relation.schema_name, relation.relname, pg_get_userbyid(relation.relowner));
        installed := installed + 1;
    END LOOP;

    IF installed = 0 THEN
        RAISE EXCEPTION 'install_owner_rls: schema % has no tables', target_schema;
    END IF;

    -- Census: what the runtime RLS guard will require of every table at boot.
    FOR relation IN
        SELECT c.oid, n.nspname AS schema_name, c.relname, c.relowner,
               c.relrowsecurity, c.relforcerowsecurity
          FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = target_schema AND c.relkind IN ('r', 'p')
    LOOP
        IF NOT relation.relrowsecurity OR NOT relation.relforcerowsecurity THEN
            RAISE EXCEPTION 'table %.% lacks ENABLE/FORCE RLS', relation.schema_name, relation.relname;
        END IF;
        IF (SELECT count(*) FROM pg_policy WHERE polrelid = relation.oid) <> 3 THEN
            RAISE EXCEPTION 'table %.% does not have exactly three RLS policies', relation.schema_name, relation.relname;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_policy WHERE polrelid = relation.oid AND polname = 'proxima_platform' AND polroles = ARRAY[relation.relowner] AND polcmd = '*') THEN
            RAISE EXCEPTION 'table %.% platform policy is not owner-role limited', relation.schema_name, relation.relname;
        END IF;
    END LOOP;
END
$install_owner_rls$;

REVOKE ALL ON FUNCTION proxima_core.install_owner_rls(text, text[], text[], text[], text[]) FROM PUBLIC;
ALTER FUNCTION proxima_core.install_owner_rls(text, text[], text[], text[], text[]) OWNER TO CURRENT_USER;

COMMENT ON FUNCTION proxima_core.install_owner_rls(text, text[], text[], text[], text[]) IS
    'Owner-RLS policies for one flavor schema from its table classification; refuses an unclassified table (docs/09 §Owner RLS).';
