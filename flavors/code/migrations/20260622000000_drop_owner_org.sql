-- S0 — Owner = Principal collapse (Track B): drop owner_org_id from proxima_code.
--
-- Full-collapse decision: org leaves the proxima_code flavor too. Same
-- DDL-drop strategy as the core 0002 migration — single-org brain, identity
-- ids/keys retained, only the org column and its composite keys removed.
--
-- Order: single-org guard → drop the runs→repos composite FK → drop/recreate
-- both repo-ingestion indexes principal-only → shrink repos_pkey +
-- repos_unique_path to principal-only → drop both owner_org_id columns.

-- 0. Single-org precondition guard — fail-closed BEFORE any DDL (mirrors the
--    core 0002 Step 0, scoped to proxima_code). Self-maintaining over every
--    owner_org_id column in the flavor schema; aborts on a multi-org brain.
DO $$
DECLARE
    union_sql     text;
    distinct_orgs bigint;
BEGIN
    SELECT string_agg(
               format('SELECT DISTINCT owner_org_id FROM %I.%I', table_schema, table_name),
               ' UNION ')
      INTO union_sql
      FROM information_schema.columns
     WHERE table_schema = 'proxima_code'
       AND column_name  = 'owner_org_id';

    IF union_sql IS NULL THEN
        RETURN;  -- no owner_org_id columns: fresh or already-collapsed schema.
    END IF;

    EXECUTE format('SELECT count(*) FROM (%s) distinct_orgs', union_sql)
      INTO distinct_orgs;

    IF distinct_orgs > 1 THEN
        RAISE EXCEPTION
            'S0 single-org precondition violated: % distinct owner_org_id values in proxima_code; DDL-drop aborted (a multi-org brain needs a re-key migration, not a column drop).',
            distinct_orgs;
    END IF;
END $$;

-- 1. Drop the composite FK (references the repos composite key).
ALTER TABLE proxima_code.repo_ingestion_runs
    DROP CONSTRAINT runs_repo_fk;

-- 2. Drop the two repo-ingestion indexes embedding owner_org_id.
DROP INDEX proxima_code.repo_ingestion_runs_by_repo;
DROP INDEX proxima_code.repo_ingestion_runs_one_active;

-- 3. Shrink the repos natural keys to principal-only.
ALTER TABLE proxima_code.repos
    DROP CONSTRAINT repos_pkey;
ALTER TABLE proxima_code.repos
    DROP CONSTRAINT repos_unique_path;
ALTER TABLE proxima_code.repos
    ADD CONSTRAINT repos_pkey
    PRIMARY KEY (owner_principal_kind, owner_principal_id, repo_id);
ALTER TABLE proxima_code.repos
    ADD CONSTRAINT repos_unique_path
    UNIQUE (owner_principal_kind, owner_principal_id, canonical_path);

-- 4. Recreate the runs→repos FK against the shrunk repos key.
ALTER TABLE proxima_code.repo_ingestion_runs
    ADD CONSTRAINT runs_repo_fk
    FOREIGN KEY (owner_principal_kind, owner_principal_id, repo_id)
    REFERENCES proxima_code.repos(owner_principal_kind, owner_principal_id, repo_id)
    ON DELETE CASCADE;

-- 5. Recreate both repo-ingestion indexes principal-only.
CREATE INDEX repo_ingestion_runs_by_repo
    ON proxima_code.repo_ingestion_runs
    USING btree (owner_principal_kind, owner_principal_id, repo_id, started_at DESC);
CREATE UNIQUE INDEX repo_ingestion_runs_one_active
    ON proxima_code.repo_ingestion_runs
    USING btree (owner_principal_kind, owner_principal_id, repo_id)
    WHERE (status = ANY (ARRAY['queued'::proxima_code.repo_ingestion_run_status, 'running'::proxima_code.repo_ingestion_run_status]));

-- 6. Drop the owner_org_id column from both tables.
ALTER TABLE proxima_code.repo_ingestion_runs DROP COLUMN owner_org_id;
ALTER TABLE proxima_code.repos DROP COLUMN owner_org_id;
