-- Additive over 0001_v008.sql and 0002_v009_declaration_triggers.sql.
--
-- INVARIANT INSTALLED: stamp ⊆ rows. A memory row that names a registered
-- memory-sidecar table in `sidecar_tables` has a row in that table, and keeps
-- it for as long as the stamp stands. That is the third and last direction of
-- the array foreign key `PostgreSQL` has no syntax for:
-- `assert_sidecar_stamp_declared` (0001) holds stamp ⊆ registry and
-- `assert_memory_declares_sidecar` (0002) holds row ⊆ stamp.
--
-- WHY IT MATTERS. A memory row stamped with a table it never wrote to cools
-- into a cold object whose sidecar dump cannot equal its own stamp, so the
-- Memory forgets and then can never be hydrated — permanently, with no error
-- at the moment the damage was done, and with no read-back that could find
-- it. Deleting the row out from under a standing stamp arrives at the same
-- state from the other end, and is strictly worse: the row's bytes are gone,
-- so nothing can repair it. This file refuses both, in the transaction that
-- would create them, which is the last point at which the writer still knows
-- what the stamp meant.
--
-- THREE TRIGGER FAMILIES, all generated:
--   1. `assert_declared_sidecar_present` — one deferred constraint trigger on
--      `proxima_core.memory` per guarded surface, refusing a stamp with no row
--      at COMMIT.
--   2. `assert_row_not_still_declared` — one deferred constraint trigger on
--      each guarded surface, refusing a DELETE that would leave the stamp
--      standing.
--   3. `<relation>_declared_by_memory_on_update` — 0002's row ⊆ stamp check
--      against a re-point of the memory key, which 0002 covered on INSERT
--      only.
--
-- ADDITIVE, deliberately, and NOT an edit to 0001 or 0002. Both are applied
-- in live databases; editing either changes the checksum of a version those
-- databases have already recorded, which `ensure_core_ledger_compatible`
-- reports as `SchemaResetRequired` — a destructive reset with no schema
-- reason. Nothing here drops, rewrites or backfills a row.
--
-- Existing rows are NOT validated by these triggers: they constrain what is
-- written from here on. An in-place upgrade of a database that already
-- carries the damage stays silent until forget, so
-- `PgSidecarRegistryFrozen::integrity_check` — which gained the matching
-- `MissingStampedSidecarRows` finding alongside this file — is the read-back
-- an operator runs once after applying it.
--
-- COST, measured on PG 18.4. The presence trigger: 8.8 µs of COMMIT per
-- stamped surface, 0.01 µs when the memory does not stamp it (the WHEN clause
-- is evaluated on the queueing path). The orphan guard: 3.0 µs of COMMIT per
-- deleted sidecar row — 1.21 s → 2.14 s on a 300k-row delete, landing on
-- owner erase. A constraint trigger must be FOR EACH ROW and cannot take a
-- transition table, so the set-based statement-level formulation that would
-- have cost one join per statement does not exist; per-row is the only shape
-- available, and it is the right price for a corruption class with no repair.


-- Stamp ⊆ rows: the shared function every presence trigger below runs.
--
-- GENERATED — see `crates/storage-pg/src/integrity.rs` and the
-- `generated_presence_triggers_are_the_migration_text` pin. Edit the
-- generator, not this block.
--
-- ONE function for every guarded table, like its 0002 sibling. Unlike that
-- one it cannot read the surface off its own relation: every presence
-- trigger is installed on `proxima_core.memory`, so the guarded surface and
-- its memory-key column both arrive as trigger arguments.
--
-- The body re-tests the membership the WHEN clause already tested. WHEN is an
-- optimisation, and an optimisation is the wrong place for the only copy of a
-- rule: a trigger carrying this function under the right name and arguments
-- but a WHEN that never matches would admit exactly what this direction
-- exists to refuse. `ensure_declaration_triggers` pins the whole rendered
-- trigger definition for the same reason.

CREATE OR REPLACE FUNCTION proxima_core.assert_declared_sidecar_present() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_ARGV[0];
    present boolean;
BEGIN
    IF NOT (surface = ANY (NEW.sidecar_tables)) THEN
        RETURN NULL;
    END IF;
    EXECUTE format('SELECT EXISTS (SELECT 1 FROM %s WHERE %I = $1)', surface, TG_ARGV[1])
       INTO present
      USING NEW.t;
    IF NOT present THEN
        RAISE EXCEPTION
            'memory % declares % in memory.sidecar_tables with no row in that table',
            NEW.t, surface
            USING ERRCODE = '23503',
                  HINT = 'a stamp with no row cools into a cold object whose sidecar dump '
                         || 'cannot equal its own stamp, so the Memory forgets and can never '
                         || 'be hydrated; write through Engine::unit_of_work, which stamps '
                         || 'exactly the tables the write inserts into, in the same '
                         || 'transaction';
    END IF;
    RETURN NULL;
END;
$$;


-- ---------------------------------------------------------------------------
-- Stamp ⊆ rows, at the other end: the shared function every orphan guard
-- below runs.
--
-- GENERATED — see `crates/storage-pg/src/integrity.rs` and the
-- `generated_presence_triggers_are_the_migration_text` pin. Edit the
-- generator, not this block.
--
-- It reads the memory key as `to_jsonb(OLD) ->> TG_ARGV[1]`, so one function
-- serves a table keyed on any column — the same move 0002's function makes on
-- NEW.

CREATE OR REPLACE FUNCTION proxima_core.assert_row_not_still_declared() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_ARGV[0];
    memory_t uuid := (to_jsonb(OLD) ->> TG_ARGV[1])::uuid;
BEGIN
    IF EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.t = memory_t
           AND m.sidecar_tables @> ARRAY[surface]
    ) THEN
        RAISE EXCEPTION
            'memory % still declares % in memory.sidecar_tables, so the row it declares there may not be deleted',
            memory_t, surface
            USING ERRCODE = '23503',
                  HINT = 'a stamp whose row was deleted cools into a cold object whose '
                         || 'sidecar dump cannot equal its own stamp, so the Memory forgets '
                         || 'and can never be hydrated, and the row is gone so nothing can '
                         || 'repair it; delete the memory row in the same transaction, which '
                         || 'is what forget and owner erase do';
    END IF;
    RETURN NULL;
END;
$$;


-- ---------------------------------------------------------------------------
-- Stamp ⊆ rows, one constraint trigger per guarded memory-sidecar table.
-- GENERATED — see `crates/storage-pg/src/integrity.rs` and the
-- `generated_presence_triggers_are_the_migration_text` pin. Edit the
-- generator, not this block.
--
-- DEFERRABLE INITIALLY DEFERRED because the memory row is inserted before the
-- sidecar rows it stamps — the sidecar's own foreign key to
-- `proxima_core.memory` requires that order — so an immediate check would
-- refuse every legitimate write. DROP + CREATE rather than CREATE OR REPLACE
-- because PostgreSQL 18 answers `CREATE OR REPLACE CONSTRAINT TRIGGER` with
-- `is not supported`; the DROP in front buys back the idempotent replay.
--
-- The WHEN clause is evaluated on the queueing path, so a memory row pays for
-- the tables it stamps and nothing for the ones it does not.
--
-- `proxima_core.mcp_call_logged_v1` is deliberately absent. It is owner-
-- pinned: it carries its own `owner_id` and no foreign key to
-- `proxima_core.memory`, exactly so a source-scoped erase can take it while
-- the Memory it records — which may since have transferred to another owner
-- — stays. Its stamp is a record of what was written, not a claim about what
-- is still there, and a presence guard would make that erase impossible.

DROP TRIGGER IF EXISTS memory_declares_proxima_core_agent_derivation_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_agent_derivation_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.agent_derivation_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.agent_derivation_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_core_agent_note_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_agent_note_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.agent_note_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.agent_note_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_core_interpretation_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_interpretation_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.interpretation_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.interpretation_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_core_utterance_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_utterance_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.utterance_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.utterance_v1', 't');

DROP TRIGGER IF EXISTS memory_declares_proxima_core_write_act_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_write_act_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.write_act_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.write_act_v1', 't');


-- ---------------------------------------------------------------------------
-- Stamp ⊆ rows against a DELETE: one constraint trigger per guarded
-- memory-sidecar table, on the table itself.
-- GENERATED — see `crates/storage-pg/src/integrity.rs` and the
-- `generated_presence_triggers_are_the_migration_text` pin. Edit the
-- generator, not this block.
--
-- DEFERRABLE INITIALLY DEFERRED, and it has to be: the sidecar's foreign key
-- to `proxima_core.memory` has no ON DELETE CASCADE, so a legitimate delete of
-- both rows must take the sidecar row FIRST. An immediate check would see the
-- memory row still standing and refuse every forget and every owner erase.
-- Deferred to COMMIT it sees the outcome: both gone, or neither.
--
-- No WHEN clause. Unlike the presence trigger, this one lives on the guarded
-- table itself, so every row it sees is a row it is responsible for.
--
-- `proxima_core.mcp_call_logged_v1` is absent here too, and for the same
-- reason: a source-scoped erase deletes its row on purpose while a
-- transferred Memory still stamps it.

DROP TRIGGER IF EXISTS agent_derivation_v1_declared_by_memory_on_delete ON proxima_core.agent_derivation_v1;
CREATE CONSTRAINT TRIGGER agent_derivation_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.agent_derivation_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.agent_derivation_v1', 't');

DROP TRIGGER IF EXISTS agent_note_v1_declared_by_memory_on_delete ON proxima_core.agent_note_v1;
CREATE CONSTRAINT TRIGGER agent_note_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.agent_note_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.agent_note_v1', 't');

DROP TRIGGER IF EXISTS interpretation_v1_declared_by_memory_on_delete ON proxima_core.interpretation_v1;
CREATE CONSTRAINT TRIGGER interpretation_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.interpretation_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.interpretation_v1', 't');

DROP TRIGGER IF EXISTS utterance_v1_declared_by_memory_on_delete ON proxima_core.utterance_v1;
CREATE CONSTRAINT TRIGGER utterance_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.utterance_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.utterance_v1', 't');

DROP TRIGGER IF EXISTS write_act_v1_declared_by_memory_on_delete ON proxima_core.write_act_v1;
CREATE CONSTRAINT TRIGGER write_act_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.write_act_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.write_act_v1', 't');


-- ---------------------------------------------------------------------------
-- Row ⊆ stamp against a re-point of the memory key. 0002 installed this
-- direction on INSERT only, so a legal row could be moved onto a memory that
-- declares nothing.
-- GENERATED — see `crates/storage-pg/src/integrity.rs` and the
-- `generated_presence_triggers_are_the_migration_text` pin. Edit the
-- generator, not this block.
--
-- These run 0002's `assert_memory_declares_sidecar` unchanged: it reads NEW,
-- which an UPDATE trigger has, and asks the same question. `UPDATE OF t`
-- rather than a bare UPDATE, because an UPDATE that leaves the key alone
-- cannot break the direction.
-- `proxima_core.mcp_call_logged_v1` IS included here: an owner-pinned row may
-- outlive its Memory, but it may never be re-pointed at another one.

CREATE OR REPLACE TRIGGER agent_derivation_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.agent_derivation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER agent_note_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.agent_note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER interpretation_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.interpretation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER mcp_call_logged_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.mcp_call_logged_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER utterance_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.utterance_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER write_act_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.write_act_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
