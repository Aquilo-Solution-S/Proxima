CREATE TYPE proxima_code.file_state AS ENUM ('Present', 'Tombstone');
CREATE TYPE proxima_code.repo_ingestion_run_status AS ENUM ('queued', 'running', 'succeeded', 'failed');
CREATE TYPE proxima_code.repo_ingestion_run_stage AS ENUM ('starting', 'facts', 'ast_edges', 'f2a', 'embeddings', 'done');
CREATE TYPE proxima_code.workspace_decision AS ENUM ('rejected', 'retry_requested', 'accepted', 'merged');
CREATE TYPE proxima_code.workspace_review_verdict AS ENUM ('approved', 'rejected', 'needs_user');

CREATE TEMP TABLE proxima_code_enum_saved_fks AS
SELECT conrelid::regclass::text AS table_name,
       conname,
       pg_get_constraintdef(oid) AS definition
  FROM pg_constraint
 WHERE contype = 'f'
   AND connamespace = 'proxima_code'::regnamespace;

DO $$
DECLARE
    fk record;
BEGIN
    FOR fk IN SELECT * FROM proxima_code_enum_saved_fks LOOP
        EXECUTE format('ALTER TABLE %s DROP CONSTRAINT %I', fk.table_name, fk.conname);
    END LOOP;
END
$$;

CREATE OR REPLACE FUNCTION proxima_code.__cast_enum_column(
    p_table_name text,
    p_column_name text,
    p_enum_type text,
    p_default_value text DEFAULT NULL
) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    IF to_regclass(p_table_name) IS NULL THEN
        RETURN;
    END IF;

    IF NOT EXISTS (
        SELECT 1
          FROM information_schema.columns
         WHERE table_schema = split_part(p_table_name, '.', 1)
           AND table_name = split_part(p_table_name, '.', 2)
           AND column_name = p_column_name
    ) THEN
        RETURN;
    END IF;

    EXECUTE format('ALTER TABLE %s ALTER COLUMN %I DROP DEFAULT', p_table_name, p_column_name);
    EXECUTE format(
        'ALTER TABLE %s ALTER COLUMN %I TYPE %s USING %I::text::%s',
        p_table_name,
        p_column_name,
        p_enum_type,
        p_column_name,
        p_enum_type
    );

    IF p_default_value IS NOT NULL THEN
        EXECUTE format(
            'ALTER TABLE %s ALTER COLUMN %I SET DEFAULT %L::%s',
            p_table_name,
            p_column_name,
            p_default_value,
            p_enum_type
        );
    END IF;
END;
$$;

ALTER TABLE IF EXISTS proxima_code.repos DROP CONSTRAINT IF EXISTS repos_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_code.repo_ingestion_runs DROP CONSTRAINT IF EXISTS runs_principal_kind_chk;
ALTER TABLE IF EXISTS proxima_code.repo_ingestion_runs DROP CONSTRAINT IF EXISTS runs_status_chk;
ALTER TABLE IF EXISTS proxima_code.repo_ingestion_runs DROP CONSTRAINT IF EXISTS runs_stage_chk;
ALTER TABLE IF EXISTS proxima_code.file_revision_v1 DROP CONSTRAINT IF EXISTS file_revision_v1_state_chk;
ALTER TABLE IF EXISTS proxima_code.code_chunk_v1 DROP CONSTRAINT IF EXISTS code_chunk_v1_state_chk;
ALTER TABLE IF EXISTS proxima_code.workspace_decision_v1 DROP CONSTRAINT IF EXISTS workspace_decision_v1_decision_chk;
ALTER TABLE IF EXISTS proxima_code.workspace_review_v1 DROP CONSTRAINT IF EXISTS workspace_review_v1_verdict_chk;

SELECT proxima_code.__cast_enum_column('proxima_code.repos', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_code.__cast_enum_column('proxima_code.repo_ingestion_runs', 'owner_principal_kind', 'proxima_core.owner_principal_kind');
SELECT proxima_code.__cast_enum_column('proxima_code.repo_ingestion_runs', 'status', 'proxima_code.repo_ingestion_run_status');
SELECT proxima_code.__cast_enum_column('proxima_code.repo_ingestion_runs', 'stage', 'proxima_code.repo_ingestion_run_stage');
SELECT proxima_code.__cast_enum_column('proxima_code.file_revision_v1', 'state', 'proxima_code.file_state');
SELECT proxima_code.__cast_enum_column('proxima_code.code_chunk_v1', 'state', 'proxima_code.file_state');
SELECT proxima_code.__cast_enum_column('proxima_code.workspace_decision_v1', 'decision', 'proxima_code.workspace_decision');
SELECT proxima_code.__cast_enum_column('proxima_code.workspace_review_v1', 'verdict', 'proxima_code.workspace_review_verdict');

DO $$
DECLARE
    fk record;
BEGIN
    FOR fk IN SELECT * FROM proxima_code_enum_saved_fks LOOP
        EXECUTE format('ALTER TABLE %s ADD CONSTRAINT %I %s', fk.table_name, fk.conname, fk.definition);
    END LOOP;
END
$$;

DROP FUNCTION proxima_code.__cast_enum_column(text, text, text, text);
