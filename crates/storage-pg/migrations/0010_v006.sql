-- Proxima core schema — v0.0.6 append-only migration (version 10).
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). 0008/0009 are the prior append-only lanes. Versions 2..7 are
-- RETIRED_PRE_V004_MIGRATION_VERSIONS (crates/storage-pg/src/lib.rs); SQLx
-- derives the version from the filename prefix, so the core sequence continues
-- at 10. Findings from the 2026-07-05 analysis (.local/analyse-2026-07-05).

-- ---------------------------------------------------------------------------
-- P1.1: embedding-job retry backoff.
-- `fail_embedding_job` stamps `next_attempt_at` with exponential backoff and
-- the claim query gates on it, so a transient provider outage (Mistral 5xx)
-- no longer burns all attempts in a hot re-claim loop. NULL means immediately
-- eligible (legacy rows + freshly-enqueued jobs).
-- ---------------------------------------------------------------------------
ALTER TABLE proxima_core.embedding_jobs
    ADD COLUMN next_attempt_at timestamp with time zone;

-- ---------------------------------------------------------------------------
-- P3-redundant-idx: drop 8 prefix-redundant btree indexes on the hottest
-- write tables. Each is a strict prefix of a same-predicate superset
-- (…_created / …_state_created), so the planner still serves prefix lookups
-- from the superset; dropping them cuts per-insert index maintenance. Mirrors
-- the deliberate GIN-index drops in 0009_v006.sql.
-- ---------------------------------------------------------------------------
DROP INDEX IF EXISTS proxima_core.idx_edges_owner;              -- prefix of idx_edges_owner_created
DROP INDEX IF EXISTS proxima_core.idx_edges_source_memory;      -- prefix of idx_edges_source_memory_created
DROP INDEX IF EXISTS proxima_core.idx_edges_source_goal;        -- prefix of idx_edges_source_goal_created
DROP INDEX IF EXISTS proxima_core.idx_edges_source_fact_entity; -- prefix of idx_edges_source_fact_entity_created
DROP INDEX IF EXISTS proxima_core.idx_edges_target_memory;      -- prefix of idx_edges_target_memory_created
DROP INDEX IF EXISTS proxima_core.idx_edges_target_goal;        -- prefix of idx_edges_target_goal_created
DROP INDEX IF EXISTS proxima_core.idx_edges_target_fact_entity; -- prefix of idx_edges_target_fact_entity_created
DROP INDEX IF EXISTS proxima_core.idx_goals_owner_state;        -- prefix of idx_goals_owner_state_created

-- ---------------------------------------------------------------------------
-- P1.8: drop the dead retention partial index. Its only consumer was the
-- retention sweep deleted in 4940295e (2026-07-01); owner fact-retention is
-- now consumer-less config metadata (enforcement deferred), so this index only
-- adds insert cost.
-- ---------------------------------------------------------------------------
DROP INDEX IF EXISTS proxima_core.idx_memories_retention_due;

-- ---------------------------------------------------------------------------
-- K6: DB-hard append-only for Facts, Abstractions, and Perspectives. Rust
-- convention plus the (line-scoped, easy-to-evade) guardrail script previously
-- protected these rows; these BEFORE UPDATE triggers enforce content, identity,
-- and provenance immutability at the database, so an admin script or faulty
-- migration cannot silently rewrite a Fact's text/payload/operator/schema —
-- the substantive append-only guarantee (analysis 2026-07-05 K6).
--
-- Two layers:
--   1. `memories` (the F/A/P row) — a column-whitelist trigger that allows
--      exactly the columns a legitimate write mutates: fact_entity_id
--      (post-ingest link), owner_kind/owner_id (publish-to-World transfer),
--      citation_mapping_id (inline citation attach), supersedes (compliance
--      erase clears it), tombstoned_at (tombstone). Every content, identity,
--      and provenance column is frozen.
--
--      `created_at` is DELIBERATELY EXCLUDED. It is temporal metadata, not
--      content, and no production path mutates it (INSERT sets now(); an
--      independent adversarial review confirmed zero production UPDATE writes
--      it). Test harnesses, however, must fabricate temporal scenarios —
--      ordering, staleness, retention/head-guard windows — for which there is
--      no production-API path to a custom created_at; freezing it would break
--      that fabrication while adding no content-integrity protection. It stays
--      guarded by convention + the line-scoped guardrail script.
--   2. The typed payload SIDECAR tables (utterance_v1, agent_note_v1, …) — a
--      generic append-only trigger that rejects EVERY column UPDATE, because a
--      Fact/Abstraction/Perspective payload is never rewritten in place (a new
--      observation is a new row). Goal sidecars (task_goal_v1) are EXCLUDED:
--      goals are the one mutable entity (state transitions). Compliance erase
--      uses DELETE, which BEFORE UPDATE triggers do not intercept, so
--      abandonment-only hard delete is unaffected.
--
-- Flavor Fact/Abstraction/Perspective/edge/citation sidecar tables must attach
-- `proxima_core.enforce_row_append_only` in their own migration (see
-- flavors/code baseline); the reusable function lives here so they can.
-- ---------------------------------------------------------------------------
CREATE FUNCTION proxima_core.memories_enforce_immutable() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.memory_id IS DISTINCT FROM OLD.memory_id
        OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
        OR NEW.schema_version IS DISTINCT FROM OLD.schema_version
        OR NEW.receipt_id IS DISTINCT FROM OLD.receipt_id
        OR NEW.kind IS DISTINCT FROM OLD.kind
        OR NEW.text IS DISTINCT FROM OLD.text
        OR NEW.operator_kind IS DISTINCT FROM OLD.operator_kind
        OR NEW.operator_id IS DISTINCT FROM OLD.operator_id
        OR NEW.input_contract_id IS DISTINCT FROM OLD.input_contract_id
        OR NEW.source_batch_id IS DISTINCT FROM OLD.source_batch_id
        OR NEW.model_id IS DISTINCT FROM OLD.model_id
        OR NEW.prompt_version IS DISTINCT FROM OLD.prompt_version
    THEN
        RAISE EXCEPTION 'memories append-only: immutable column changed on memory_id=%', OLD.memory_id;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER memories_enforce_immutable
    BEFORE UPDATE ON proxima_core.memories
    FOR EACH ROW EXECUTE FUNCTION proxima_core.memories_enforce_immutable();

-- Generic append-only guard for immutable typed payload sidecars. Any UPDATE
-- raises; INSERT (incl. ON CONFLICT DO NOTHING) and DELETE (compliance erase)
-- are untouched.
CREATE FUNCTION proxima_core.enforce_row_append_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    RAISE EXCEPTION 'append-only: % is immutable (UPDATE rejected)', TG_TABLE_NAME;
END;
$$;

CREATE TRIGGER agent_note_v1_append_only BEFORE UPDATE ON proxima_core.agent_note_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER utterance_v1_append_only BEFORE UPDATE ON proxima_core.utterance_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER mcp_call_logged_v1_append_only BEFORE UPDATE ON proxima_core.mcp_call_logged_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER goal_activated_v1_append_only BEFORE UPDATE ON proxima_core.goal_activated_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER goal_paused_v1_append_only BEFORE UPDATE ON proxima_core.goal_paused_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER goal_achieved_v1_append_only BEFORE UPDATE ON proxima_core.goal_achieved_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER goal_abandoned_v1_append_only BEFORE UPDATE ON proxima_core.goal_abandoned_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER agent_derivation_v1_append_only BEFORE UPDATE ON proxima_core.agent_derivation_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER agent_link_v1_append_only BEFORE UPDATE ON proxima_core.agent_link_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER cited_uploaded_blob_v1_append_only BEFORE UPDATE ON proxima_core.cited_uploaded_blob_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
CREATE TRIGGER cited_mcp_call_io_v1_append_only BEFORE UPDATE ON proxima_core.cited_mcp_call_io_v1
    FOR EACH ROW EXECUTE FUNCTION proxima_core.enforce_row_append_only();
