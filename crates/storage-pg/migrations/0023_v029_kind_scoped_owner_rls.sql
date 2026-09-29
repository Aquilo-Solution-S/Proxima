-- Every owner-keyed table follows one rule: a scope may act on kind K of owner
-- O when its limit for that direction is at least K (Causa.Authorization).
--
-- bind_owner_scope binds one owner list per kind and direction. Until this
-- file only memory, goal and the FK sidecars read them; every other
-- owner-keyed table filtered on the Fact list, so a Fact-only scope saw an
-- owner's Abstraction, Perspective and Goal rows in memory_head, sketch,
-- cooled, announce, content and the embedding tables.
--
-- Classification. Every proxima_core table is in exactly one class; a table
-- in none refuses the migration.
--   kind column      cooled, memory_head, sketch: the row's own kind
--   memory row       embedding_heads, embedding_jobs, embeddings (entity_id),
--                    projection (memory_id): that memory's kind; an
--                    embedding row of a Goal (a Goal is embeddable) needs
--                    the Goal list
--   goal             goal_head, wake_config
--   announce         Goal, or the kind of the hot or cooled memory at its t
--   content          the kind of a hot or cooled admission naming it; an
--                    insert needs only Fact write, since it precedes the
--                    admission
--   closed_handle    the kind of its memory_head
--   settled          memory, goal and the FK sidecars (per kind since 0014);
--                    Fact-only and owner-level rows on the Fact lists;
--                    metadata and ownerless tables
--
-- A limit covers every lower kind, so the lists nest: an owner on the
-- Perspective list may act on every Memory kind and skips the parent lookup
-- for announce and content, whose rows only a Memory row names; on the
-- memory-row tables only the Goal list skips it. Every other scope looks the
-- kind up, and a row whose memory is gone reads as no kind at all.
--
-- install_owner_rls: memory_owner_tables now follow the parent memory's kind,
-- keyed by the table's FK to proxima_core.memory (else its t column), and may
-- carry an owner_id. A flavor applies this by calling the installer again.

DO $kind_rls$
DECLARE
    kind_tables constant text[] := ARRAY['cooled', 'memory_head', 'sketch'];
    memory_tables constant jsonb := '{"embedding_heads": "entity_id",
        "embedding_jobs": "entity_id", "embeddings": "entity_id",
        "projection": "memory_id"}';
    goal_tables constant text[] := ARRAY['goal_head', 'wake_config'];
    settled constant text[] := ARRAY[
        'memory', 'goal',
        'agent_derivation_v1', 'agent_note_v1', 'goal_replay_declaration',
        'interpretation_v1', 'task_goal_v1', 'utterance_v1', 'write_act_v1',
        'blob', 'blob_uploads', 'cold_purge_pending', 'delegated_authority_grants',
        'group_memberships', 'ingest_keys', 'mcp_call_logged_v1', 'owners',
        'publication_origin', 'publication_outbox', 'source_cursors',
        'flavor_surface', 'lexical_default', 'lexical_languages',
        'erased_pin_target', 'installation', 'owner_rls_epoch'
    ];
    settings constant jsonb := '{
        "read": ["app.owner", "app.read_abstraction", "app.read_perspective", "app.read_goal"],
        "write": ["app.write_owner", "app.write_abstraction", "app.write_perspective", "app.write_goal"]}';
    relation record;
    direction text;
    owners text[];
    kind_of text;
    expr jsonb := '{}';
    table_name text;
BEGIN
    FOR relation IN
        SELECT c.relname
          FROM pg_class AS c
          JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = 'proxima_core' AND c.relkind IN ('r', 'p')
    LOOP
        IF relation.relname <> ALL(kind_tables || goal_tables || settled
                || ARRAY(SELECT jsonb_object_keys(memory_tables))
                || ARRAY['announce', 'content', 'closed_handle']) THEN
            RAISE EXCEPTION 'owner RLS classification missing for proxima_core.%', relation.relname;
        END IF;
    END LOOP;

    FOREACH direction IN ARRAY ARRAY['read', 'write'] LOOP
        -- owners[1..4]: the Fact, Abstraction, Perspective and Goal lists.
        owners := ARRAY(
            SELECT format('(SELECT COALESCE(NULLIF(current_setting(%L, true), %L)::uuid[], %L::uuid[]))::uuid[]',
                          setting, '', '{}')
              FROM jsonb_array_elements_text(settings -> direction) WITH ORDINALITY AS s(setting, n)
             ORDER BY n);
        -- The list for the kind named by the %s expression.
        kind_of := format('CASE %%s::text WHEN ''fact'' THEN %s WHEN ''abstraction'' THEN %s WHEN ''perspective'' THEN %s ELSE %s END',
                          owners[1], owners[2], owners[3], owners[4]);

        FOREACH table_name IN ARRAY kind_tables LOOP
            expr := jsonb_set(expr, ARRAY[direction || ':' || table_name], to_jsonb(format(
                'owner_id = ANY(%s) AND owner_id = ANY(%s)',
                owners[1], format(kind_of, 'kind'))), true);
        END LOOP;
        FOR table_name IN SELECT jsonb_object_keys(memory_tables) LOOP
            expr := jsonb_set(expr, ARRAY[direction || ':' || table_name], to_jsonb(format(
                'owner_id = ANY(%s) OR (owner_id = ANY(%s) AND EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = proxima_core.%I.%I AND parent.owner_id = ANY(%s)))',
                owners[4], owners[1], table_name, memory_tables ->> table_name,
                format(kind_of, 'parent.kind'))), true);
        END LOOP;
        FOREACH table_name IN ARRAY goal_tables LOOP
            expr := jsonb_set(expr, ARRAY[direction || ':' || table_name],
                to_jsonb(format('owner_id = ANY(%s)', owners[4])), true);
        END LOOP;
        expr := jsonb_set(expr, ARRAY[direction || ':announce'], to_jsonb(format(
            'CASE WHEN entity = ''goal'' THEN owner_id = ANY(%s) ELSE owner_id = ANY(%s) OR (owner_id = ANY(%s) AND (EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = proxima_core.announce.t AND parent.owner_id = ANY(%s)) OR EXISTS (SELECT 1 FROM proxima_core.cooled AS parent WHERE parent.t = proxima_core.announce.t AND parent.owner_id = ANY(%s)))) END',
            owners[4], owners[3], owners[1], format(kind_of, 'parent.kind'),
            format(kind_of, 'parent.kind'))), true);
        expr := jsonb_set(expr, ARRAY[direction || ':content'], to_jsonb(format(
            'owner_id = ANY(%s) OR (owner_id = ANY(%s) AND (EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.content_id = proxima_core.content.content_id AND parent.owner_id = ANY(%s)) OR EXISTS (SELECT 1 FROM proxima_core.cooled AS parent WHERE parent.content_id = proxima_core.content.content_id AND parent.owner_id = ANY(%s))))',
            owners[3], owners[1], format(kind_of, 'parent.kind'),
            format(kind_of, 'parent.kind'))), true);
        expr := jsonb_set(expr, ARRAY[direction || ':closed_handle'], to_jsonb(format(
            'EXISTS (SELECT 1 FROM proxima_core.memory_head AS parent WHERE parent.handle = proxima_core.closed_handle.handle AND parent.owner_id = ANY(%s))',
            format(kind_of, 'parent.kind'))), true);
        IF direction = 'write' THEN
            -- Content is written before the admission that names it.
            expr := jsonb_set(expr, ARRAY['check:content'],
                to_jsonb(format('owner_id = ANY(%s)', owners[1])), true);
        END IF;
    END LOOP;

    FOR table_name IN
        SELECT DISTINCT split_part(key, ':', 2) FROM jsonb_object_keys(expr) AS key
    LOOP
        EXECUTE format('ALTER POLICY proxima_owner_read ON proxima_core.%I USING (%s)',
                       table_name, expr ->> ('read:' || table_name));
        EXECUTE format('ALTER POLICY proxima_owner_write ON proxima_core.%I USING (%s) WITH CHECK (%s)',
                       table_name, expr ->> ('write:' || table_name),
                       COALESCE(expr ->> ('check:' || table_name), expr ->> ('write:' || table_name)));
    END LOOP;
END
$kind_rls$;

-- install_owner_rls, 0018's body with one class changed:
--   memory_owner_tables  rows of one proxima_core.memory row, keyed by the
--                        table's single-column FK to it on its leading
--                        primary-key column, else its only FK to it when no
--                        other t column competes, else its uuid t; any other
--                        shape is refused as ambiguous. Read and write follow
--                        that memory's kind. May carry an owner_id (a
--                        projection keyed by memory_id).
-- A flavor that listed tables here keeps its old policies until it calls the
-- installer again.
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
    fk record;
    parents jsonb := '{}';
    fk_child text;
    fk_schema text;
    fk_table text;
    fk_column text;
    chain text;
    hops integer;
    parent_has_t boolean;
    memory_key text;
    memory_keys integer;
    memory_on_key boolean;
    kind_of text;
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
        SELECT child_att.attname AS child_column,
               parent_ns.nspname AS parent_schema,
               parent.relname AS parent_table,
               parent_att.attname AS parent_column,
               COALESCE(con.conkey[1] = pk.conkey[1], false) AS on_key,
               count(*) OVER () AS candidates
          INTO fk
          FROM pg_constraint AS con
          JOIN pg_attribute AS child_att ON child_att.attrelid = con.conrelid AND child_att.attnum = con.conkey[1]
          JOIN pg_class AS parent ON parent.oid = con.confrelid
          JOIN pg_namespace AS parent_ns ON parent_ns.oid = parent.relnamespace
          JOIN pg_attribute AS parent_att ON parent_att.attrelid = parent.oid AND parent_att.attnum = con.confkey[1]
          LEFT JOIN pg_constraint AS pk ON pk.conrelid = con.conrelid AND pk.contype = 'p'
         WHERE con.conrelid = relation.oid AND con.contype = 'f'
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
            RAISE EXCEPTION 'owner RLS classification missing for %.%: no single-column FK to proxima_core or %',
                target_schema, relation.relname, target_schema;
        END IF;
        IF NOT fk.on_key AND fk.candidates > 1 THEN
            RAISE EXCEPTION 'install_owner_rls: %.% has % candidate parent FKs and none on its leading primary-key column; its owner is ambiguous',
                target_schema, relation.relname, fk.candidates;
        END IF;
        parents := parents || jsonb_build_object(relation.relname, to_jsonb(fk));
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
        IF relation.has_owner_id AND relation.relname <> ALL(COALESCE(owner_id_tables, '{}')
                || COALESCE(memory_owner_tables, '{}')) THEN
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
            -- The memory this row belongs to: the FK on the leading
            -- primary-key column, else the only FK to memory when no other t
            -- column competes, else t.
            SELECT child_att.attname, count(*) OVER (),
                   COALESCE(con.conkey[1] = pk.conkey[1], false)
              INTO memory_key, memory_keys, memory_on_key
              FROM pg_constraint AS con
              JOIN pg_attribute AS child_att ON child_att.attrelid = con.conrelid AND child_att.attnum = con.conkey[1]
              LEFT JOIN pg_constraint AS pk ON pk.conrelid = con.conrelid AND pk.contype = 'p'
             WHERE con.conrelid = relation.oid AND con.contype = 'f'
               AND con.confrelid = 'proxima_core.memory'::regclass
               AND array_length(con.conkey, 1) = 1
             ORDER BY COALESCE(con.conkey[1] = pk.conkey[1], false) DESC, con.oid
             LIMIT 1;
            IF memory_key IS NOT NULL AND NOT memory_on_key
               AND (memory_keys > 1 OR (memory_key <> 't' AND EXISTS (
                    SELECT 1 FROM pg_attribute
                     WHERE attrelid = relation.oid AND attname = 't' AND attnum > 0 AND NOT attisdropped)))
            THEN
                RAISE EXCEPTION 'install_owner_rls: %.% has % foreign key(s) to proxima_core.memory, none on its leading primary-key column, beside a t column or each other; its memory is ambiguous',
                    relation.schema_name, relation.relname, memory_keys;
            END IF;
            memory_key := COALESCE(memory_key, 't');
            IF NOT EXISTS (
                SELECT 1 FROM pg_attribute
                 WHERE attrelid = relation.oid AND attname = memory_key AND atttypid = 'uuid'::regtype
                   AND attnum > 0 AND NOT attisdropped
            ) THEN
                RAISE EXCEPTION 'install_owner_rls: %.% is in memory_owner_tables but has no uuid t column',
                    relation.schema_name, relation.relname;
            END IF;
            -- The parent memory's kind picks the list, read and write alike.
            FOREACH listed IN ARRAY ARRAY['read', 'write'] LOOP
                kind_of := format('CASE parent.kind::text WHEN ''fact'' THEN %s WHEN ''abstraction'' THEN %s ELSE %s END',
                    format('(SELECT COALESCE(NULLIF(current_setting(%L, true), %L)::uuid[], %L::uuid[]))::uuid[]',
                           CASE listed WHEN 'read' THEN 'app.owner' ELSE 'app.write_owner' END, '', '{}'),
                    format('(SELECT COALESCE(NULLIF(current_setting(%L, true), %L)::uuid[], %L::uuid[]))::uuid[]',
                           'app.' || listed || '_abstraction', '', '{}'),
                    format('(SELECT COALESCE(NULLIF(current_setting(%L, true), %L)::uuid[], %L::uuid[]))::uuid[]',
                           'app.' || listed || '_perspective', '', '{}'));
                kind_of := format('EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = %I.%I.%I AND parent.owner_id = ANY(%s))',
                    relation.schema_name, relation.relname, memory_key, kind_of);
                IF listed = 'read' THEN
                    read_expr := kind_of;
                ELSE
                    write_expr := kind_of;
                END IF;
            END LOOP;
        ELSIF relation.relname = ANY(COALESCE(ownerless_tables, '{}')) THEN
            read_expr := 'false';
            write_expr := 'false';
        ELSE
            fk_child := parents -> relation.relname ->> 'child_column';
            fk_schema := parents -> relation.relname ->> 'parent_schema';
            fk_table := parents -> relation.relname ->> 'parent_table';
            fk_column := parents -> relation.relname ->> 'parent_column';
            SELECT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid = format('%I.%I', fk_schema, fk_table)::regclass AND attname = 't' AND attnum > 0 AND NOT attisdropped) INTO parent_has_t;
            read_expr := format('EXISTS (SELECT 1 FROM %I.%I AS parent WHERE parent.%I = %I.%I.%I AND current_setting(''app.proxima_scope'', true) = ''owner'')', fk_schema, fk_table, fk_column, relation.schema_name, relation.relname, fk_child);
            IF fk_schema = 'proxima_core' AND fk_table = 'memory' THEN
                write_expr := format('EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.%I = %I.%I.%I AND ((parent.kind = ''fact'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''abstraction'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''perspective'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))))', fk_column, relation.schema_name, relation.relname, fk_child);
            ELSIF fk_schema = 'proxima_core' AND fk_table = 'goal' THEN
                write_expr := format('EXISTS (SELECT 1 FROM proxima_core.goal AS parent WHERE parent.%I = %I.%I.%I AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_goal'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))', fk_column, relation.schema_name, relation.relname, fk_child);
            ELSIF parent_has_t THEN
                write_expr := format('EXISTS (SELECT 1 FROM %I.%I AS parent JOIN proxima_core.memory AS owner_memory ON owner_memory.t = parent.t WHERE parent.%I = %I.%I.%I AND ((owner_memory.kind = ''fact'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (owner_memory.kind = ''abstraction'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (owner_memory.kind = ''perspective'' AND owner_memory.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))))', fk_schema, fk_table, fk_column, relation.schema_name, relation.relname, fk_child);
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
