-- v0.0.9 for the code flavor, additive over `20260818000020_v008_baseline.sql`.
--
-- INVARIANT INSTALLED: the flavor half of core's
-- `0002_v009_declaration_triggers.sql` — one `BEFORE INSERT` trigger per
-- registered memory-sidecar table of this flavor, so a row here exists only
-- for a memory whose `sidecar_tables` declares this table.
--
-- ADDITIVE, deliberately. The v008 baseline is frozen: changing its bytes
-- changes the checksum of a version live databases have already applied and
-- resets every one of them. `CREATE OR REPLACE` throughout, so this replays
-- as a no-op on a fresh database and upgrades a live v0.0.8 one in place.
--
-- The shared function is deliberately NOT restated here. It is core's,
-- defined once, and a flavor that redefined it would be a second declaration
-- of one thing. This migration therefore requires core's 0002 to have run,
-- which the composite binary guarantees: core migrations run first, then each
-- linked flavor's.
--
-- GENERATED — see `crates/storage-pg/src/integrity.rs` and the
-- `generated_declaration_triggers_are_the_code_migration_text` pin. Edit the
-- generator, not this block.
--
-- The child tables (`acceptance_criterion_v1`, `code_chunk_call_v1`,
-- `execution_plan_item_v1`, `test_requested_criterion_v1`) carry none: they
-- hang off a parent sidecar row rather than off a memory, so no
-- `memory.sidecar_tables` names them.

CREATE OR REPLACE TRIGGER acceptance_criteria_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.acceptance_criteria_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER acceptance_summary_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.acceptance_summary_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER acceptance_verification_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.acceptance_verification_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER code_chunk_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.code_chunk_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER commit_summarizer_self_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.commit_summarizer_self_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER commit_summary_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.commit_summary_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER commit_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.commit_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER development_perspective_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.development_perspective_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER engineer_self_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.engineer_self_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER execution_plan_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.execution_plan_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER execution_result_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.execution_result_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER file_revision_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.file_revision_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER test_requested_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.test_requested_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER test_result_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.test_result_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER work_assignment_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.work_assignment_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER work_requested_v1_declared_by_memory
    BEFORE INSERT ON proxima_code.work_requested_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
