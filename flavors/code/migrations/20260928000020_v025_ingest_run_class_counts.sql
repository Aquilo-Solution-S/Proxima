-- v0.0.25: per-class counts on an ingestion run (#370).
--
-- The ingest tool report already counts files and chunks by class.
-- `repo_ingestion_runs` is what `proxima-code_get_ingest_run` reads after
-- a background ingest, and it kept only the flat totals. Eight integer
-- columns, default 0: a run written before this file, and a run that has
-- not succeeded, both read as zeros. No rewrite of `code_chunk_v1`.

ALTER TABLE proxima_code.repo_ingestion_runs
    ADD COLUMN files_source integer DEFAULT 0 NOT NULL,
    ADD COLUMN files_generated integer DEFAULT 0 NOT NULL,
    ADD COLUMN files_vendored integer DEFAULT 0 NOT NULL,
    ADD COLUMN files_lockfile integer DEFAULT 0 NOT NULL,
    ADD COLUMN chunks_source integer DEFAULT 0 NOT NULL,
    ADD COLUMN chunks_generated integer DEFAULT 0 NOT NULL,
    ADD COLUMN chunks_vendored integer DEFAULT 0 NOT NULL,
    ADD COLUMN chunks_lockfile integer DEFAULT 0 NOT NULL;

COMMENT ON COLUMN proxima_code.repo_ingestion_runs.files_source IS 'Present source files this run derived chunks for. 0 until the run succeeds.';
COMMENT ON COLUMN proxima_code.repo_ingestion_runs.files_generated IS 'Present generated files this run derived chunks for. 0 until the run succeeds.';
COMMENT ON COLUMN proxima_code.repo_ingestion_runs.files_vendored IS 'Present vendored files this run derived chunks for. 0 until the run succeeds.';
COMMENT ON COLUMN proxima_code.repo_ingestion_runs.files_lockfile IS 'Present lockfiles this run derived chunks for. 0 until the run succeeds.';
COMMENT ON COLUMN proxima_code.repo_ingestion_runs.chunks_source IS 'Chunks this run emitted from source files. 0 until the run succeeds.';
COMMENT ON COLUMN proxima_code.repo_ingestion_runs.chunks_generated IS 'Chunks this run emitted from generated files. 0 until the run succeeds.';
COMMENT ON COLUMN proxima_code.repo_ingestion_runs.chunks_vendored IS 'Chunks this run emitted from vendored files. 0 until the run succeeds.';
COMMENT ON COLUMN proxima_code.repo_ingestion_runs.chunks_lockfile IS 'Chunks this run emitted from lockfiles. 0 until the run succeeds.';
