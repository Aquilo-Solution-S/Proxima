-- Drop the Code-flavor workspace-review subsystem.
--
-- The autonomous workspace runner is gone (see core migration
-- 20260523000010_drop_workspace_mode.sql). With no producer for these
-- rows, the supporting Code-flavor tables and enum types go too.
--
-- Renumbered from 20260523000010: substrate and flavor migrators share
-- one _sqlx_migrations table, and the core migration above already owns
-- that version. Safe to replay — every statement is idempotent.

DROP TABLE IF EXISTS proxima_code.workspace_decision_v1;
DROP TABLE IF EXISTS proxima_code.workspace_review_v1;
DROP TABLE IF EXISTS proxima_code.workspace_run_v1;

DROP TYPE IF EXISTS proxima_code.workspace_decision;
DROP TYPE IF EXISTS proxima_code.workspace_review_verdict;
