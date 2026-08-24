-- v0.0.9, additive over the v0.0.8 baseline.
--
-- INVARIANT INSTALLED: a row in a registered memory-sidecar table exists only
-- for a memory row whose `sidecar_tables` declares that table. That is the
-- other direction of the array foreign key `assert_sidecar_stamp_declared`
-- already enforces (stamp ⊆ registry); this half is row ⊆ stamp.
--
-- ADDITIVE, deliberately. `0001_v008.sql` is a frozen baseline: editing it
-- changes the checksum of a version existing databases have already applied,
-- which sends every one of them into `SchemaResetRequired` — a destructive
-- reset with no schema reason. Nothing here drops, rewrites or backfills:
-- `CREATE OR REPLACE` throughout, so this file is a no-op replay on a fresh
-- database that just ran 0001 and an in-place upgrade on a live v0.0.8 one.
--
-- Existing rows are NOT validated. The triggers are `BEFORE INSERT`, so they
-- constrain what is written from here on; a v0.0.8 database that already
-- carries undeclared sidecar rows keeps them, and
-- `PgSidecarRegistryFrozen::integrity_check` is the tool that finds them.

-- Row ⊆ stamp, the other direction of the same array foreign key, enforced
-- at write time on every registered memory-sidecar table.
--
-- GENERATED — see `crates/storage-pg/src/integrity.rs` and the
-- `generated_declaration_triggers_are_the_migration_text` pin. Edit the
-- generator, not this block.
--
-- ONE function for every guarded table: the surface is the table the
-- trigger is installed on (`TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME`) and
-- the memory-key column arrives as `TG_ARGV[0]`, so neither is a second
-- declaration that could drift. The triggers that call it are emitted per
-- table, below for core and in each flavor's own v0.0.9 migration for that
-- flavor's tables.
CREATE OR REPLACE FUNCTION proxima_core.assert_memory_declares_sidecar() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME;
    memory_t uuid := (to_jsonb(NEW) ->> TG_ARGV[0])::uuid;
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.t = memory_t
           AND m.sidecar_tables @> ARRAY[surface]
    ) THEN
        RAISE EXCEPTION
            'sidecar row in % for memory %, which does not declare % in memory.sidecar_tables',
            surface, memory_t, surface
            USING ERRCODE = '23503',
                  HINT = 'forget, owner erase and owner export all reach a sidecar row '
                         || 'through memory.sidecar_tables and nowhere else, so an undeclared '
                         || 'row is reachable by none of them; write through '
                         || 'Engine::unit_of_work, which stamps the memory row with every '
                         || 'table the write touches, in the same transaction and before the '
                         || 'sidecar row';
    END IF;
    RETURN NEW;
END;
$$;


-- ---------------------------------------------------------------------------
-- Declaration integrity, one trigger per registered memory-sidecar table.
-- GENERATED — see `crates/storage-pg/src/integrity.rs` and the
-- `generated_declaration_triggers_are_the_migration_text` pin. Edit the
-- generator, not this block.
--
-- `proxima_core.task_goal_v1` is deliberately absent: it hangs off a Goal,
-- not off a Memory, so no `memory.sidecar_tables` ever names it. The
-- projection tables are absent for the same reason the projection block
-- says — a projection row is derived from a sidecar row, never stamped.

CREATE OR REPLACE TRIGGER agent_derivation_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.agent_derivation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER agent_note_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.agent_note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER interpretation_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.interpretation_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER mcp_call_logged_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.mcp_call_logged_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER utterance_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.utterance_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

CREATE OR REPLACE TRIGGER write_act_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.write_act_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');
