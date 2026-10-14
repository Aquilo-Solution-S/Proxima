-- v0.0.30: an owner is one owner id, a UUID. A repo and its ingestion runs
-- key on (owner_id, repo_id); the redundant owner kind goes from their keys,
-- indexes and foreign key. Re-running the file changes nothing.

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'proxima_code'
           AND table_name = 'repos'
           AND column_name = 'owner_kind'
    ) THEN
        ALTER TABLE proxima_code.repo_ingestion_runs DROP CONSTRAINT runs_repo_fk;
        DROP INDEX proxima_code.repo_ingestion_runs_by_repo;
        DROP INDEX proxima_code.repo_ingestion_runs_one_active;
        ALTER TABLE proxima_code.repos DROP CONSTRAINT repos_unique_path;
        ALTER TABLE proxima_code.repos DROP CONSTRAINT repos_pkey;

        ALTER TABLE proxima_code.repo_ingestion_runs DROP COLUMN owner_kind;
        ALTER TABLE proxima_code.repos DROP COLUMN owner_kind;

        ALTER TABLE proxima_code.repos
            ADD CONSTRAINT repos_pkey PRIMARY KEY (owner_id, repo_id);
        ALTER TABLE proxima_code.repos
            ADD CONSTRAINT repos_unique_path UNIQUE (owner_id, canonical_path);
        CREATE INDEX repo_ingestion_runs_by_repo
            ON proxima_code.repo_ingestion_runs
            USING btree (owner_id, repo_id, started_at DESC);
        CREATE UNIQUE INDEX repo_ingestion_runs_one_active
            ON proxima_code.repo_ingestion_runs
            USING btree (owner_id, repo_id)
            WHERE (status = ANY (ARRAY[
                'queued'::proxima_code.repo_ingestion_run_status,
                'running'::proxima_code.repo_ingestion_run_status
            ]));
        ALTER TABLE proxima_code.repo_ingestion_runs
            ADD CONSTRAINT runs_repo_fk FOREIGN KEY (owner_id, repo_id)
            REFERENCES proxima_code.repos (owner_id, repo_id) ON DELETE CASCADE;
    END IF;
END
$$;
