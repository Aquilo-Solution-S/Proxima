-- Code flavor — v0.0.6 append-only lane (analysis 2026-07-05, K6).
--
-- Core migration 0010 makes the F/A/P `memories` row and every CORE typed
-- payload sidecar DB-hard append-only, and ships the reusable guard
-- `proxima_core.enforce_row_append_only` for flavors to attach to their own
-- Fact/Abstraction/Perspective/edge payload sidecars. Core migrators run before
-- flavor migrators (crates/proxima/src/migrations.rs), so the function exists
-- here.
--
-- These are the code flavor's immutable payload sidecars (and their normalized
-- child rows): a Fact/Abstraction/Perspective payload is never rewritten in
-- place — a new observation is a new row. The MUTABLE operational tables
-- (proxima_code.repos, proxima_code.repo_ingestion_runs) are DELIBERATELY
-- EXCLUDED: they hold repo config and ingestion-run cursors that legitimately
-- UPDATE. Compliance erase uses DELETE, which BEFORE UPDATE does not intercept.

-- Fact sidecars
CREATE TRIGGER commit_v1_append_only BEFORE UPDATE ON proxima_code.commit_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER file_revision_v1_append_only BEFORE UPDATE ON proxima_code.file_revision_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER work_requested_v1_append_only BEFORE UPDATE ON proxima_code.work_requested_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER test_requested_v1_append_only BEFORE UPDATE ON proxima_code.test_requested_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER test_requested_criterion_v1_append_only BEFORE UPDATE ON proxima_code.test_requested_criterion_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER acceptance_criteria_v1_append_only BEFORE UPDATE ON proxima_code.acceptance_criteria_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER acceptance_criterion_v1_append_only BEFORE UPDATE ON proxima_code.acceptance_criterion_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER execution_result_v1_append_only BEFORE UPDATE ON proxima_code.execution_result_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER test_result_v1_append_only BEFORE UPDATE ON proxima_code.test_result_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER acceptance_verification_v1_append_only BEFORE UPDATE ON proxima_code.acceptance_verification_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();

-- Abstraction sidecars
CREATE TRIGGER code_chunk_v1_append_only BEFORE UPDATE ON proxima_code.code_chunk_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER commit_summary_v1_append_only BEFORE UPDATE ON proxima_code.commit_summary_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER execution_plan_v1_append_only BEFORE UPDATE ON proxima_code.execution_plan_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER execution_plan_item_v1_append_only BEFORE UPDATE ON proxima_code.execution_plan_item_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER acceptance_summary_v1_append_only BEFORE UPDATE ON proxima_code.acceptance_summary_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();

-- Perspective sidecars
CREATE TRIGGER development_perspective_v1_append_only BEFORE UPDATE ON proxima_code.development_perspective_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER commit_summarizer_self_v1_append_only BEFORE UPDATE ON proxima_code.commit_summarizer_self_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER engineer_self_v1_append_only BEFORE UPDATE ON proxima_code.engineer_self_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();

-- Edge sidecar
CREATE TRIGGER code_calls_v1_append_only BEFORE UPDATE ON proxima_code.code_calls_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
