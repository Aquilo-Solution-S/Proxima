-- The code flavor's half of core's `0009_declared_sidecar_presence.sql`,
-- additive over `20260818000020_v008_baseline.sql` and
-- `20260824000020_v009_declaration_triggers.sql`.
--
-- INVARIANT INSTALLED: the flavor half of stamp ⊆ rows — a memory row that
-- names one of this flavor's sidecar tables in `sidecar_tables` has a row in
-- it, and keeps it for as long as the stamp stands — and the flavor half of
-- row ⊆ stamp against an UPDATE of the memory-key column, which core's `0002`
-- guards on INSERT only.
--
-- WHERE THE DDL LIVES, and one deliberate exception. A flavor migration
-- normally only creates objects on tables its own migrations created, and the
-- orphan guards and UPDATE guards below obey that: they sit on this flavor's
-- own sidecar tables. The presence triggers cannot. A presence trigger fires
-- on the memory row, so `proxima_core.memory` is the only relation it can
-- live on, and one trigger per guarded surface is what keeps the WHEN clause
-- able to name a single table. That is safe here because one DDL-capable role
-- applies both lanes and owns both schemas (`docs/15-deployment.md`), and
-- because the trigger names carry the guarded schema — `memory_declares_
-- proxima_code_<relation>` cannot collide with core's.
--
-- The shared functions stay core's — `proxima_core.assert_declared_sidecar_
-- present`, `proxima_core.assert_row_not_still_declared` and
-- `proxima_core.assert_memory_declares_sidecar`, each defined once, in core —
-- and this file carries only the triggers that call them.
--
-- GENERATED — see `crates/storage-pg/src/integrity.rs` and the
-- `generated_presence_triggers_are_the_code_migration_text` pin. Edit the
-- generator, not this file.
--
-- ADDITIVE, deliberately: the baseline and the v0.0.9 declaration migration
-- are applied in live databases, and editing either changes the checksum of a
-- version those databases have already recorded. Nothing here drops, rewrites
-- or backfills a row, and existing rows are NOT validated —
-- `PgSidecarRegistryFrozen::integrity_check` is what finds those.


-- ---------------------------------------------------------------------------
-- Stamp ⊆ rows, one constraint trigger per sidecar table this flavor
-- registers. DEFERRABLE INITIALLY DEFERRED because the memory row is inserted
-- before the sidecar rows it stamps. DROP + CREATE because PostgreSQL 18
-- answers `CREATE OR REPLACE CONSTRAINT TRIGGER` with `is not supported`.
-- The WHEN clause is evaluated on the queueing path, so a memory row pays for
-- the tables it stamps and nothing for the ones it does not.
--
-- No sidecar of this flavor is owner-pinned, so every one of them is guarded.

DROP TRIGGER IF EXISTS memory_declares_proxima_code_acceptance_criteria_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_acceptance_criteria_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.acceptance_criteria_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.acceptance_criteria_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_acceptance_summary_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_acceptance_summary_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.acceptance_summary_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.acceptance_summary_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_acceptance_verification_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_acceptance_verification_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.acceptance_verification_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.acceptance_verification_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_code_chunk_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_code_chunk_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.code_chunk_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.code_chunk_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_commit_summarizer_self_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_commit_summarizer_self_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.commit_summarizer_self_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.commit_summarizer_self_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_commit_summary_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_commit_summary_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.commit_summary_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.commit_summary_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_commit_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_commit_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.commit_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.commit_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_development_perspective_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_development_perspective_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.development_perspective_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.development_perspective_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_engineer_self_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_engineer_self_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.engineer_self_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.engineer_self_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_execution_plan_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_execution_plan_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.execution_plan_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.execution_plan_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_execution_result_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_execution_result_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.execution_result_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.execution_result_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_file_revision_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_file_revision_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.file_revision_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.file_revision_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_test_requested_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_test_requested_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.test_requested_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.test_requested_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_test_result_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_test_result_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.test_result_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.test_result_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_work_assignment_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_work_assignment_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.work_assignment_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.work_assignment_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_code_work_requested_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_code_work_requested_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_code.work_requested_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_code.work_requested_v1', 't');


-- ---------------------------------------------------------------------------
-- Stamp ⊆ rows against a DELETE: one constraint trigger per sidecar table,
-- on the table itself, refusing a delete that would leave the stamp standing.
--
-- DEFERRABLE INITIALLY DEFERRED, and it has to be: the sidecar's foreign key
-- to `proxima_core.memory` has no ON DELETE CASCADE, so a legitimate delete of
-- both rows must take the sidecar row FIRST. An immediate check would see the
-- memory row still standing and refuse every forget and every erase.
--
-- No WHEN clause: this one lives on the guarded table itself, so every row it
-- sees is a row it is responsible for.

DROP TRIGGER IF EXISTS acceptance_criteria_v1_declared_by_memory_on_delete ON proxima_code.acceptance_criteria_v1;
CREATE CONSTRAINT TRIGGER acceptance_criteria_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.acceptance_criteria_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.acceptance_criteria_v1', 't');

DROP TRIGGER IF EXISTS acceptance_summary_v1_declared_by_memory_on_delete ON proxima_code.acceptance_summary_v1;
CREATE CONSTRAINT TRIGGER acceptance_summary_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.acceptance_summary_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.acceptance_summary_v1', 't');

DROP TRIGGER IF EXISTS acceptance_verification_v1_declared_by_memory_on_delete ON proxima_code.acceptance_verification_v1;
CREATE CONSTRAINT TRIGGER acceptance_verification_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.acceptance_verification_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.acceptance_verification_v1', 't');

DROP TRIGGER IF EXISTS code_chunk_v1_declared_by_memory_on_delete ON proxima_code.code_chunk_v1;
CREATE CONSTRAINT TRIGGER code_chunk_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.code_chunk_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.code_chunk_v1', 't');

DROP TRIGGER IF EXISTS commit_summarizer_self_v1_declared_by_memory_on_delete ON proxima_code.commit_summarizer_self_v1;
CREATE CONSTRAINT TRIGGER commit_summarizer_self_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.commit_summarizer_self_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.commit_summarizer_self_v1', 't');

DROP TRIGGER IF EXISTS commit_summary_v1_declared_by_memory_on_delete ON proxima_code.commit_summary_v1;
CREATE CONSTRAINT TRIGGER commit_summary_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.commit_summary_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.commit_summary_v1', 't');

DROP TRIGGER IF EXISTS commit_v1_declared_by_memory_on_delete ON proxima_code.commit_v1;
CREATE CONSTRAINT TRIGGER commit_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.commit_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.commit_v1', 't');

DROP TRIGGER IF EXISTS development_perspective_v1_declared_by_memory_on_delete ON proxima_code.development_perspective_v1;
CREATE CONSTRAINT TRIGGER development_perspective_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.development_perspective_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.development_perspective_v1', 't');

DROP TRIGGER IF EXISTS engineer_self_v1_declared_by_memory_on_delete ON proxima_code.engineer_self_v1;
CREATE CONSTRAINT TRIGGER engineer_self_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.engineer_self_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.engineer_self_v1', 't');

DROP TRIGGER IF EXISTS execution_plan_v1_declared_by_memory_on_delete ON proxima_code.execution_plan_v1;
CREATE CONSTRAINT TRIGGER execution_plan_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.execution_plan_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.execution_plan_v1', 't');

DROP TRIGGER IF EXISTS execution_result_v1_declared_by_memory_on_delete ON proxima_code.execution_result_v1;
CREATE CONSTRAINT TRIGGER execution_result_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.execution_result_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.execution_result_v1', 't');

DROP TRIGGER IF EXISTS file_revision_v1_declared_by_memory_on_delete ON proxima_code.file_revision_v1;
CREATE CONSTRAINT TRIGGER file_revision_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.file_revision_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.file_revision_v1', 't');

DROP TRIGGER IF EXISTS test_requested_v1_declared_by_memory_on_delete ON proxima_code.test_requested_v1;
CREATE CONSTRAINT TRIGGER test_requested_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.test_requested_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.test_requested_v1', 't');

DROP TRIGGER IF EXISTS test_result_v1_declared_by_memory_on_delete ON proxima_code.test_result_v1;
CREATE CONSTRAINT TRIGGER test_result_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.test_result_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.test_result_v1', 't');

DROP TRIGGER IF EXISTS work_assignment_v1_declared_by_memory_on_delete ON proxima_code.work_assignment_v1;
CREATE CONSTRAINT TRIGGER work_assignment_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.work_assignment_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.work_assignment_v1', 't');

DROP TRIGGER IF EXISTS work_requested_v1_declared_by_memory_on_delete ON proxima_code.work_requested_v1;
CREATE CONSTRAINT TRIGGER work_requested_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_code.work_requested_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_code.work_requested_v1', 't');


-- ---------------------------------------------------------------------------
-- Row ⊆ stamp against a re-point of the memory key. Core's `0002` installed
-- this direction on INSERT only, so a legal row could be moved onto a memory
-- that declares nothing.
--
-- These run `proxima_core.assert_memory_declares_sidecar` unchanged: it reads
-- NEW, which an UPDATE trigger has, and asks the same question. `UPDATE OF
-- <key>` rather than a bare UPDATE, because an UPDATE that leaves the key
-- alone cannot break the direction.

CREATE OR REPLACE TRIGGER acceptance_criteria_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.acceptance_criteria_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER acceptance_summary_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.acceptance_summary_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER acceptance_verification_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.acceptance_verification_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER code_chunk_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.code_chunk_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER commit_summarizer_self_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.commit_summarizer_self_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER commit_summary_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.commit_summary_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER commit_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.commit_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER development_perspective_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.development_perspective_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER engineer_self_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.engineer_self_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER execution_plan_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.execution_plan_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER execution_result_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.execution_result_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER file_revision_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.file_revision_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER test_requested_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.test_requested_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER test_result_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.test_result_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER work_assignment_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.work_assignment_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER work_requested_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_code.work_requested_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
