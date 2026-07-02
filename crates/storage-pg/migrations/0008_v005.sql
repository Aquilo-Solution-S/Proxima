-- Proxima core schema — v0.0.5 append-only migration.
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline: sqlx records its checksum in
-- every existing database, so it must NEVER be edited (any edit fails every
-- deployed database's boot with VersionMismatch / V004ResetRequired). All
-- post-v0.0.4 DDL lands here, appended section by section.
--
-- VERSION NUMBER NOTE: this file is version 8, not 2. SQLx derives the
-- migration version from the filename prefix, and versions 2..7 (plus
-- 20260622000000) are RETIRED_PRE_V004_MIGRATION_VERSIONS
-- (crates/storage-pg/src/lib.rs): the ensure_v004_baseline_compatible boot
-- preflight fail-closes any database whose _sqlx_migrations table records one
-- of those versions, and the guarded dev-migrate reset deletes exactly those
-- rows. Reusing version 2 would make every database that applied this
-- migration flag itself as pre-v0.0.4-stale on next boot. Retired version
-- numbers are burned forever; the core sequence continues at 8.

-- ---------------------------------------------------------------------------
-- Task 4: publish-to-World constraint correction
-- ---------------------------------------------------------------------------
-- Kernel law: publish-to-World is an owner TRANSFER to OwnerRef::World — not
-- an ACL flag, not a share row. World is universally readable and never a
-- write/manage authority. The v0.0.4 baseline stamped every owner-bearing
-- table with a blanket world_not_write_owner_chk (owner_kind <> 'world'),
-- which on the two entity home-row tables (memories, goals) contradicted the
-- kernel: World-OWNED rows must be representable, because they are the
-- persisted result of a deliberate Engine::publish_to_world owner transfer.
--
-- Scope: memories and goals ONLY — the two row families the transfer moves
-- and the only tables consulted by the read-visibility gates
-- (OwnerAccessReadPort::home_owner / visible_to_any). Every other
-- owner-shadow table (edges, embeddings, embedding_heads, embedding_jobs,
-- fact_receipts, fact_entities, citation_mappings, cited_objects,
-- cited_object_uploads, change_event, owner_fact_retention, source_batches)
-- KEEPS its world_not_write_owner_chk: rows there attribute a write/author
-- action, and World must never appear as a write attribution.
--
-- No replacement CHECK is added. "World is never writable/manageable" is a
-- property of write AUTHORIZATION, not of row shape — a row cannot
-- distinguish "freshly written under World" (forbidden) from "transferred to
-- World" (the publish verb). The engine's authorize_write gate
-- (crates/core/src/engine/pipeline.rs, `resolved == world()` short-circuit)
-- is the enforcement point that denies every fresh write and every re-publish
-- under a World owner. The remaining row-shape invariant is still enforced by
-- the untouched memories_owner_ref_shape_chk / goals_owner_ref_shape_chk:
-- (world => owner_id IS NULL) AND (personal|group => owner_id IS NOT NULL).

ALTER TABLE proxima_core.memories
    DROP CONSTRAINT memories_world_not_write_owner_chk;

ALTER TABLE proxima_core.goals
    DROP CONSTRAINT goals_world_not_write_owner_chk;

COMMENT ON TABLE proxima_core.memories IS
  'Graph nodes of kind Fact | Abstraction | Perspective (the fourth node kind, Goal, lives in goals). Discriminated by the kind column via memories_variant_chk: Fact rows have kind NULL, optional receipt_id, optional citation_mapping_id, and no operator fields; Abstraction (FtoA operator) and Perspective (AtoP operator) = kind set, operator-derived (operator_kind/model_id/prompt_version), with no receipt_id or citation. See docs/02-memory.md for the Fact -> Abstraction -> Perspective -> Goal derivation pipeline. Unlike every owner-shadow table (edges, embeddings, fact_receipts, change_event, ...), this table intentionally carries no world_not_write_owner_chk since v0.0.5: owner_kind = world is the persisted result of a deliberate publish-to-World owner transfer (Engine::publish_to_world), not a fresh write under World. memories_owner_ref_shape_chk still enforces the owner_kind/owner_id shape; the engine authz layer (authorize_write) is the sole enforcement point blocking fresh writes with a World owner.';

COMMENT ON TABLE proxima_core.goals IS
  'The Goal node kind (desired end-states), kept out of memories because it carries a lifecycle and authorship model. Goal topology is ordinary proxima_core.edges. See docs/06-goals-and-self.md. Unlike every owner-shadow table (edges, embeddings, fact_receipts, change_event, ...), this table intentionally carries no world_not_write_owner_chk since v0.0.5: owner_kind = world is the persisted result of a deliberate publish-to-World owner transfer (Engine::publish_to_world), not a fresh write under World. goals_owner_ref_shape_chk still enforces the owner_kind/owner_id shape; the engine authz layer (authorize_write) is the sole enforcement point blocking fresh writes with a World owner.';

-- ---------------------------------------------------------------------------
-- Task 11: owner-scoped opaque source cursors
-- ---------------------------------------------------------------------------
-- Crash-safe resume positions for host-owned evidence projectors. Cursor bytes
-- are opaque to Proxima and round-trip as bytea; only owner/source identity and
-- updated_at are substrate-visible.

CREATE TABLE proxima_core.source_cursors (
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    source text NOT NULL,
    cursor bytea NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT source_cursors_owner_ref_shape_chk CHECK (((owner_kind = 'world'::proxima_core.owner_ref_kind AND owner_id IS NULL) OR (owner_kind IN ('personal'::proxima_core.owner_ref_kind, 'group'::proxima_core.owner_ref_kind) AND owner_id IS NOT NULL))),
    CONSTRAINT source_cursors_world_not_write_owner_chk CHECK ((owner_kind <> 'world'::proxima_core.owner_ref_kind)),
    CONSTRAINT source_cursors_pkey PRIMARY KEY (owner_kind, owner_id, source)
);

COMMENT ON TABLE proxima_core.source_cursors IS
  'Owner-scoped opaque source resume cursors for host projectors. Proxima persists and returns cursor bytea verbatim; it never interprets, validates, decodes, normalizes, or derives ordering from cursor bytes. Fresh writes remain engine-authorized owner writes; World cannot own cursor rows.';
