-- v0.0.15 Code owner-RLS cutover, applied after the core migration.
-- Stop every older pack sharing this schema before activation.

DO $code_owner_rls$
DECLARE
    relation record;
    fk record;
    parent_has_t boolean;
    owner_role text;
    read_expr text;
    write_expr text;
    ownerless boolean;
    ownerless_tables constant text[] := ARRAY[]::text[];
    approved_fk_tables constant text[] := ARRAY[
        'proxima_code.acceptance_criteria_v1', 'proxima_code.acceptance_criterion_v1',
        'proxima_code.acceptance_summary_v1', 'proxima_code.acceptance_verification_v1',
        'proxima_code.code_chunk_call_v1', 'proxima_code.code_chunk_v1',
        'proxima_code.development_perspective_v1', 'proxima_code.execution_plan_item_v1',
        'proxima_code.execution_plan_v1', 'proxima_code.execution_result_v1',
        'proxima_code.file_revision_v1', 'proxima_code.commit_summary_v1',
        'proxima_code.commit_v1', 'proxima_code.repo_ingestion_runs', 'proxima_code.repos',
        'proxima_code.test_requested_criterion_v1', 'proxima_code.test_requested_v1',
        'proxima_code.test_result_v1', 'proxima_code.work_assignment_v1',
        'proxima_code.work_requested_v1'
    ];
    relation_key text;
BEGIN
    FOR relation IN
        SELECT c.oid, n.nspname AS schema_name, c.relname, c.relowner,
               EXISTS (
                   SELECT 1 FROM pg_attribute AS a
                    WHERE a.attrelid = c.oid AND a.attname = 'owner_id'
                      AND NOT a.attisdropped AND a.attnum > 0
               ) AS has_owner_id
          FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = 'proxima_code' AND c.relkind IN ('r', 'p')
         ORDER BY c.relname
    LOOP
        owner_role := pg_get_userbyid(relation.relowner);
        relation_key := format('%I.%I', relation.schema_name, relation.relname);
        ownerless := relation_key = ANY(ownerless_tables);
        IF relation.has_owner_id THEN
            read_expr := $expr$owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
            write_expr := $expr$owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])$expr$;
        ELSIF relation.relname IN ('commit_summarizer_self_v1', 'engineer_self_v1') THEN
            read_expr := format('EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = %I.%I.t AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))', relation.schema_name, relation.relname);
            write_expr := format('EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = %I.%I.t AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))', relation.schema_name, relation.relname);
        ELSIF ownerless THEN
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
              JOIN pg_attribute AS child_att ON child_att.attrelid = relation.oid AND child_att.attnum = con.conkey[1]
              JOIN pg_class AS parent ON parent.oid = con.confrelid
              JOIN pg_namespace AS parent_ns ON parent_ns.oid = parent.relnamespace
              JOIN pg_attribute AS parent_att ON parent_att.attrelid = parent.oid AND parent_att.attnum = con.confkey[1]
             WHERE con.conrelid = relation.oid AND con.contype = 'f'
               AND array_length(con.conkey, 1) = 1 AND array_length(con.confkey, 1) = 1
               AND parent_ns.nspname IN ('proxima_core', 'proxima_code')
             ORDER BY (parent.relname IN ('memory', 'goal')) DESC, con.oid
             LIMIT 1;
            IF fk IS NULL THEN
                RAISE EXCEPTION 'owner RLS classification missing for %.%', relation.schema_name, relation.relname;
            END IF;
            SELECT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid = format('%I.%I', fk.parent_schema, fk.parent_table)::regclass AND attname = 't' AND attnum > 0 AND NOT attisdropped) INTO parent_has_t;
            read_expr := format('EXISTS (SELECT 1 FROM %I.%I AS parent WHERE parent.%I = %I.%I.%I AND current_setting(''app.proxima_scope'', true) = ''owner'')', fk.parent_schema, fk.parent_table, fk.parent_column, relation.schema_name, relation.relname, fk.child_column);
            IF fk.parent_table = 'memory' THEN
                write_expr := format('EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.%I = %I.%I.%I AND ((parent.kind = ''fact'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_owner'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''abstraction'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_abstraction'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[])) OR (parent.kind = ''perspective'' AND parent.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting(''app.write_perspective'', true), '''')::uuid[], ''{}''::uuid[]))::uuid[]))))', fk.parent_column, relation.schema_name, relation.relname, fk.child_column);
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
        EXECUTE format('CREATE POLICY proxima_platform ON %I.%I FOR ALL TO %I USING (current_setting(''app.proxima_scope'', true) = ''platform'') WITH CHECK (current_setting(''app.proxima_scope'', true) = ''platform'')', relation.schema_name, relation.relname, owner_role);
    END LOOP;
END
$code_owner_rls$;

DO $code_owner_rls_census$
DECLARE relation record;
BEGIN
    FOR relation IN
        SELECT c.oid, n.nspname AS schema_name, c.relname, c.relowner,
               c.relrowsecurity, c.relforcerowsecurity
          FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
         WHERE n.nspname = 'proxima_code' AND c.relkind IN ('r', 'p')
    LOOP
        IF NOT relation.relrowsecurity OR NOT relation.relforcerowsecurity THEN
            RAISE EXCEPTION 'code table %.% lacks ENABLE/FORCE RLS', relation.schema_name, relation.relname;
        END IF;
        IF (SELECT count(*) FROM pg_policy WHERE polrelid = relation.oid) <> 3 THEN
            RAISE EXCEPTION 'code table %.% does not have exactly three RLS policies', relation.schema_name, relation.relname;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_policy WHERE polrelid = relation.oid AND polname = 'proxima_platform' AND polroles = ARRAY[relation.relowner] AND polcmd = '*') THEN
            RAISE EXCEPTION 'code table %.% platform policy is not owner-role limited', relation.schema_name, relation.relname;
        END IF;
    END LOOP;
END
$code_owner_rls_census$;
