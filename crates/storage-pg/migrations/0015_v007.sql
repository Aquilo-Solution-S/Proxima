-- Proxima core schema — v0.0.7 append-only migration (version 15).
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). 0008..0014 are the prior append-only lanes.

-- ---------------------------------------------------------------------------
-- A source-ingest edge is unique in the database, not merely by convention.
--
-- doc/07 §ID Types and the kernel's `EdgeIdAuthorshipValid` state an iff: a
-- `SourceIngest` edge carries a content-derived id, every other authorship
-- carries a fresh UUIDv7. The point of that rule is that re-ingesting a
-- source converges on the edges it already wrote. Until now nothing enforced
-- it — `edges` carried exactly one unique key, the `edge_id` primary key, so
-- `ON CONFLICT (edge_id) DO NOTHING` in the append path could only ever fire
-- on an id the writer had already made deterministic. A writer that forgot,
-- or a derivation that stopped covering a field, produced silent duplicates
-- that no read could see: `walk_memory_lineage` deduplicates its nodes, so
-- only `count(*)` distinguished one edge from four.
--
-- Restricted to `SourceIngest` because duplicate edges are LEGITIMATE for
-- every other authorship, and a table-wide constraint would destroy data:
--
--   * `proxima-code/calls` writes one edge per call SITE, so a caller that
--     calls the same callee twice is two rows identical in every column
--     named below, told apart by their ids and their typed sidecars.
--   * `core_link` is documented as non-idempotent; two agent links A→B with
--     different `reason`/`confidence` are two claims, not one written twice.
--
-- So the predicate is the kernel's own split, and nothing else changes.
--
-- NULLS NOT DISTINCT (PG 15+; this repo builds and tests on PG 18) is
-- load-bearing. Each endpoint is a triple of nullable columns with a CHECK
-- that exactly one is non-null, so under the SQL default — where NULL is
-- distinct from NULL — this index would admit every duplicate it exists to
-- refuse and would look like it was working.
--
-- `authorship_owner_memory_id` is in the key: it is caller-supplied, and two
-- different self-perspectives asserting the same link are different claims.
-- `relation_class` is not: it is bound from the relation's registered
-- descriptor on every insert, so it adds no discriminating power.
--
-- No dedupe step precedes this. Creating the index FAILS on a database that
-- already holds duplicate source-ingest edges, and that is the intended
-- behaviour — this repo has no precedent for a migration that silently
-- deletes rows, and edges are documented as append-by-construction. A
-- failure here is a report, not a corruption. In practice the set is empty:
-- no in-tree production path has ever written a `SourceIngest` edge. A
-- flavor can — `append_owner_checked_typed_edge` takes the authorship kind
-- as an argument — and if one did so with a random id, this is where that
-- surfaces.

CREATE UNIQUE INDEX edges_source_ingest_identity_uq
    ON proxima_core.edges (
        owner_kind,
        owner_id,
        relation,
        source_kind,
        source_memory_id,
        source_goal_id,
        source_fact_entity_id,
        target_kind,
        target_memory_id,
        target_goal_id,
        target_fact_entity_id,
        authorship_kind,
        authorship_owner_memory_id
    )
    NULLS NOT DISTINCT
    WHERE authorship_kind = 'SourceIngest';

COMMENT ON INDEX proxima_core.edges_source_ingest_identity_uq IS
    'One source-ingest edge per identity. The append path keeps ON CONFLICT '
    '(edge_id), so this index does not silence a duplicate — it RAISES when '
    'the content-derived edge id stops covering the identity columns, '
    'turning a drifted derivation into a failed write instead of a second row.';
