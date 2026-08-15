-- Proxima core schema — v0.0.8 draft migration (version 17): sweep
-- read-path indexes (sql-sweep findings S3 + S8).
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). 0008/0009/0010/0011/0016 are the prior append-only lanes. This file
-- is a v0.0.8-cycle DRAFT (docs/how-to/migrations.md): squash at release
-- preparation under a fresh version number, together with 0016 and 0018.
--
-- These are plain CREATE INDEX, so each build takes a SHARE lock and blocks
-- writes to its table for the duration. Migrations run inside a transaction
-- and CREATE INDEX CONCURRENTLY cannot, so a concurrent build is not
-- expressible in this lane today; adding one is an open item
-- (docs/how-to/migrations.md). Operators upgrading a large `memories` or
-- `change_event` should expect a write pause here.

-- ---------------------------------------------------------------------------
-- Missing FK-referencing-column indexes (finding S3).
-- Five FK constraints have no supporting index on the referencing column, so
-- every DELETE of a referenced row fires an RI check that seq-scans the whole
-- referencing table. `record_utterance` mints one `source_batches` row per
-- utterance, which makes an owner-scope compliance erase O(N²) over memories
-- (measured on a 2,000-parent/300,000-child scratch corpus: parent DELETE
-- 18,442ms without the index, 7.2ms with it). Partial `WHERE col IS NOT NULL`
-- because the RI probe is always `col = $1`, which implies NOT NULL, and the
-- majority of rows carry NULL here (PostgreSQL docs §5.5.5 Foreign Keys,
-- §11.8 Partial Indexes).
-- ---------------------------------------------------------------------------
CREATE INDEX idx_fact_entities_current_memory
    ON proxima_core.fact_entities (current_memory_id)
    WHERE current_memory_id IS NOT NULL;

CREATE INDEX idx_goals_assignment_perspective
    ON proxima_core.goals (assignment_perspective_id)
    WHERE assignment_perspective_id IS NOT NULL;

CREATE INDEX idx_memories_citation_mapping
    ON proxima_core.memories (citation_mapping_id)
    WHERE citation_mapping_id IS NOT NULL;

CREATE INDEX idx_memories_source_batch
    ON proxima_core.memories (source_batch_id)
    WHERE source_batch_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- change_event entity-id indexes (finding S8; the entity_goal_id index is
-- also S3's fifth missing FK index — change_event_entity_goal_id_fkey).
-- The idempotency-replay probes (`fact_replay_outcome`, `persist_mcp_call`,
-- goal insert replay) all run `WHERE entity_memory_id = $1 ORDER BY seq ASC
-- LIMIT 1` (or the entity_goal_id twin) against a table indexed only by
-- (seq) and (owner_kind, owner_id, …) — a full scan + sort of the hottest
-- insert table on exactly the path idempotency keys exist to make cheap.
-- Trailing `seq` serves the ORDER BY … LIMIT 1 as an index-only top-1
-- (PostgreSQL docs §11.3, §11.9); partial because change_event_endpoint_chk
-- proves exactly one of the two columns is non-NULL on entity rows and both
-- are NULL on edge rows.
-- ---------------------------------------------------------------------------
CREATE INDEX idx_change_event_entity_memory_seq
    ON proxima_core.change_event (entity_memory_id, seq)
    WHERE entity_memory_id IS NOT NULL;

CREATE INDEX idx_change_event_entity_goal_seq
    ON proxima_core.change_event (entity_goal_id, seq)
    WHERE entity_goal_id IS NOT NULL;
