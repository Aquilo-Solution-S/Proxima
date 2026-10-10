-- v0.0.30: the owner-RLS installer, one table per call.
--
-- install_owner_rls wants every table of a schema classified in one call and
-- rewrites them all. A host that adds one table per migration either restated
-- its whole schema each time or copied the policy statements by hand, and the
-- copies drifted (0018). This file adds the same policies for one table:
--
--   install_owner_rls_table(schema, table, class[, owner_column])
--       ENABLE + FORCE RLS on the table and its three policies; no other
--       table is read or written. Calling it twice leaves the same policies.
--   assert_owner_rls_census(schema)
--       the census install_owner_rls ends with, on every base table of the
--       schema; raises on the first table without the policies. A migration
--       that adds several tables ends with it, which gives back at migration
--       time the property that a forgotten table raises.
--
-- Classes (owner_rls_class) are the four lists of install_owner_rls; a fifth
-- is a new migration:
--   owner_id      a uuid owner column (default owner_id, a proxima_core.owners
--                 id): read app.owner, write app.write_owner. A text column is
--                 refused; the canonical personal:<uuid> / group:<uuid> string
--                 is an external convention, a host stores
--                 OwnerRef::stable_key_uuid() in a uuid column instead.
--   fk_parent     reaches its owner through one FK (0018's rule: the FK on the
--                 leading primary-key column, else the only one).
--   ownerless     platform-only: owner scope reads and writes nothing.
--   memory_owner  rows of one proxima_core.memory row (0023's rule).
--
-- Both installers build their policies in owner_rls_apply_class, the class
-- code of 0023 moved out of install_owner_rls unchanged, and resolve a parent
-- FK in owner_rls_parent_fk. install_owner_rls is redefined on top of them:
-- signature, whole-schema checks, census and every expression it builds are
-- the ones 0023 shipped. A flavor that installed a table through either keeps
-- its policies until it calls an installer again.
--
-- Per table the cycle check cannot see the other tables of the call. It
-- follows the resolved parent FK through every in-schema parent without an
-- owner_id column and refuses every chain install_owner_rls refuses; it also
-- refuses a parent of another class whose FK leads back into the chain.
--
-- Every function is SECURITY INVOKER with EXECUTE for its owner only: they run
-- DDL as the migration role, which must own the tables.

CREATE TYPE proxima_core.owner_rls_class AS ENUM (
    'owner_id', 'fk_parent', 'ownerless', 'memory_owner'
);

-- The FK a table reaches its owner through, or NULL when there is none. The
-- FK on the leading primary-key column wins, else the memory/goal parent, else
-- the oldest; `candidates` and `on_key` let the caller refuse an ambiguous
-- table. Partition clones of a parent-level FK are skipped.
CREATE FUNCTION proxima_core.owner_rls_find_parent_fk(
    target_schema text,
    target_table text
)
RETURNS jsonb
LANGUAGE plpgsql
VOLATILE
SECURITY INVOKER
SET search_path = pg_catalog
AS $find_parent_fk$
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
$find_parent_fk$;

-- owner_rls_find_parent_fk for a table that must have exactly one owner-bearing
-- FK: none, or several with none on the leading primary-key column, raise.
CREATE FUNCTION proxima_core.owner_rls_parent_fk(
    caller text,
    target_schema text,
    target_table text
)
RETURNS jsonb
LANGUAGE plpgsql
VOLATILE
SECURITY INVOKER
SET search_path = pg_catalog
AS $parent_fk$
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
$parent_fk$;

-- The policies of one class on one table: resolve what the class reads, build
-- the read and write expressions, ENABLE + FORCE RLS and replace the three
-- policies. `caller` prefixes the refusals; `class_label` names the class the
-- way the caller does ('in memory_owner_tables', 'class memory_owner'). The
-- callers have checked the table and, for owner_id, its owner column.
CREATE FUNCTION proxima_core.owner_rls_apply_class(
    caller text,
    class_label text,
    target_schema text,
    target_table text,
    class proxima_core.owner_rls_class,
    owner_column text
)
RETURNS void
LANGUAGE plpgsql
VOLATILE
SECURITY INVOKER
SET search_path = pg_catalog
AS $apply_class$
DECLARE
    rel_oid oid;
    rel_owner name;
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
    kind_of text;
    read_expr text;
    write_expr text;
BEGIN
    SELECT c.oid, pg_get_userbyid(c.relowner) INTO rel_oid, rel_owner
      FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
     WHERE n.nspname = target_schema AND c.relname = target_table AND c.relkind IN ('r', 'p');

    CASE class
        WHEN 'owner_id' THEN
            read_expr := format($expr$%I = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$, owner_column);
            write_expr := format($expr$%I = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$, owner_column);
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

    EXECUTE format('ALTER TABLE %I.%I ENABLE ROW LEVEL SECURITY', target_schema, target_table);
    EXECUTE format('ALTER TABLE %I.%I FORCE ROW LEVEL SECURITY', target_schema, target_table);
    EXECUTE format('DROP POLICY IF EXISTS proxima_owner_read ON %I.%I', target_schema, target_table);
    EXECUTE format('DROP POLICY IF EXISTS proxima_owner_write ON %I.%I', target_schema, target_table);
    EXECUTE format('DROP POLICY IF EXISTS proxima_platform ON %I.%I', target_schema, target_table);
    EXECUTE format('CREATE POLICY proxima_owner_read ON %I.%I FOR SELECT TO PUBLIC USING (%s)', target_schema, target_table, read_expr);
    EXECUTE format('CREATE POLICY proxima_owner_write ON %I.%I FOR ALL TO PUBLIC USING (%s) WITH CHECK (%s)', target_schema, target_table, write_expr, write_expr);
    EXECUTE format('CREATE POLICY proxima_platform ON %I.%I FOR ALL TO %I USING (current_setting(''app.proxima_scope'', true) = ''platform'') WITH CHECK (current_setting(''app.proxima_scope'', true) = ''platform'')', target_schema, target_table, rel_owner);
END
$apply_class$;

-- What the runtime RLS guard will require of one table at boot: both RLS
-- flags, exactly three policies, the platform policy on the table owner role.
CREATE FUNCTION proxima_core.owner_rls_census_table(
    target_schema text,
    target_table text
)
RETURNS void
LANGUAGE plpgsql
VOLATILE
SECURITY INVOKER
SET search_path = pg_catalog
AS $census_table$
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
$census_table$;

CREATE FUNCTION proxima_core.assert_owner_rls_census(target_schema text)
RETURNS void
LANGUAGE plpgsql
VOLATILE
SECURITY INVOKER
SET search_path = pg_catalog
AS $assert_census$
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
$assert_census$;

CREATE FUNCTION proxima_core.install_owner_rls_table(
    target_schema text,
    target_table text,
    class proxima_core.owner_rls_class,
    owner_column text DEFAULT 'owner_id'
)
RETURNS void
LANGUAGE plpgsql
VOLATILE
SECURITY INVOKER
SET search_path = pg_catalog
AS $install_owner_rls_table$
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
$install_owner_rls_table$;

-- install_owner_rls, 0023's body with the class code moved to
-- owner_rls_apply_class, the parent-FK lookup to owner_rls_parent_fk and the
-- census to assert_owner_rls_census. Its signature, checks and policies are
-- unchanged.
CREATE OR REPLACE FUNCTION proxima_core.install_owner_rls(
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
$install_owner_rls$;

REVOKE ALL ON FUNCTION proxima_core.owner_rls_find_parent_fk(text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.owner_rls_parent_fk(text, text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.owner_rls_apply_class(text, text, text, text, proxima_core.owner_rls_class, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.owner_rls_census_table(text, text) FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.assert_owner_rls_census(text) FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.install_owner_rls_table(text, text, proxima_core.owner_rls_class, text) FROM PUBLIC;
ALTER FUNCTION proxima_core.owner_rls_find_parent_fk(text, text) OWNER TO CURRENT_USER;
ALTER FUNCTION proxima_core.owner_rls_parent_fk(text, text, text) OWNER TO CURRENT_USER;
ALTER FUNCTION proxima_core.owner_rls_apply_class(text, text, text, text, proxima_core.owner_rls_class, text) OWNER TO CURRENT_USER;
ALTER FUNCTION proxima_core.owner_rls_census_table(text, text) OWNER TO CURRENT_USER;
ALTER FUNCTION proxima_core.assert_owner_rls_census(text) OWNER TO CURRENT_USER;
ALTER FUNCTION proxima_core.install_owner_rls_table(text, text, proxima_core.owner_rls_class, text) OWNER TO CURRENT_USER;

COMMENT ON TYPE proxima_core.owner_rls_class IS
    'The four owner-RLS policy classes; a fifth is a new migration (docs/09 §Owner RLS).';
COMMENT ON FUNCTION proxima_core.install_owner_rls_table(text, text, proxima_core.owner_rls_class, text) IS
    'Owner-RLS policies for one table of a flavor or host schema; the per-table form of install_owner_rls (docs/09 §Owner RLS).';
COMMENT ON FUNCTION proxima_core.assert_owner_rls_census(text) IS
    'Raises on the first base table of the schema without ENABLE/FORCE RLS, exactly three policies and the owner-role platform policy.';
