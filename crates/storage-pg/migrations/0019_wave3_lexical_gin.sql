-- Proxima core schema — v0.0.8 draft migration (version 19): the GIN
-- indexes the lexical read path can finally select.
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). This file is a v0.0.8-cycle DRAFT (docs/how-to/migrations.md):
-- squash at release preparation under a fresh version number, together
-- with 0016, 0017 and 0018. Same non-concurrent build caveat as those:
-- `CREATE INDEX CONCURRENTLY` cannot run inside a transaction and sqlx
-- runs each migration in one, so these builds hold a write lock on their
-- table for the duration. Measured on a ~10^5-row single-owner corpus of
-- conversational text: 1.1 s for the `memories` index, which is 45 MB
-- against a 140 MB table.
--
-- ---------------------------------------------------------------------------
-- Why this reverses a decision two migrations made deliberately.
--
-- 0009_v006.sql dropped the v0.0.6 sidecar GIN indexes, and 0011_v007.sql
-- declined to add one for the stored `search_tsv` columns it introduced.
-- Both gave the same reason and the reason was correct at the time: the
-- read path matched `c.search_tsv` against an owner-scoped `candidates`
-- CTE, and no index on a base table can serve a predicate applied to a CTE
-- result. 0011 went further and argued the index would buy nothing anyway:
--
--     owner-first enumeration already reduces a search to a few hundred
--     rows before any text predicate runs
--
-- That is the premise that has since turned out to be false. It holds for
-- a multi-tenant deployment where each owner holds a small slice of the
-- table. It fails completely for a single-owner deployment, where the
-- owner scope IS the table — and that is the shape a personal memory
-- system has by default. Measured there, the lexical branch read the whole
-- owner scope and spilled a tsvector per candidate row to disk on every
-- single search.
--
-- So this migration ships together with the read-path restructure that
-- makes it selectable (`verbs::query::search`): the match predicate now
-- sits on the base tables, beside each branch's owner predicate, instead
-- of above a materialised CTE. An index without that restructure is pure
-- write amplification — which is precisely what 0009 deleted. Neither half
-- may be shipped without the other.
--
-- Measured on the same corpus, hybrid mode (the product default), lexical
-- leg only, relative to the shipped statement: index alone 1.00, restructure
-- alone 0.82, both together 0.0004.
--
-- ---------------------------------------------------------------------------
-- Which columns get one.
--
-- A column earns an index iff a pushed-down branch matches `@@` against it.
-- `memories` is the base branch and is always present; `agent_note_v1` and
-- `agent_derivation_v1` declare `tsv_column: Some("search_tsv")` in their
-- `search_projection()`, so the builder emits `s.search_tsv @@ …` for them.
--
-- `interpretation_v1` deliberately gets none: it declares no `tsv_column`,
-- so the builder tokenises inline
-- (`proxima_core.lexical_tsv(m.lexical_language, <claim concat>)`) and no
-- index on the raw table could match that expression — the same
-- expression-index brittleness 0009 deleted. Its `UNION ALL` arm scans that
-- one sidecar; because the arms are planned separately, that does not stop
-- the memories and note arms from using theirs. Giving it a stored column
-- is a separate change with a table rewrite attached.
--
-- These are plain, not partial. `push_base_memory_filters` does emit
-- `m.tombstoned_at IS NULL` unconditionally today, so a partial index would
-- be selectable, but the coupling buys only the tombstoned minority's rows
-- and silently stops paying the day a request shape omits that predicate.
-- ---------------------------------------------------------------------------
CREATE INDEX idx_memories_search_tsv
    ON proxima_core.memories USING gin (search_tsv);

CREATE INDEX idx_agent_note_v1_search_tsv
    ON proxima_core.agent_note_v1 USING gin (search_tsv);

CREATE INDEX idx_agent_derivation_v1_search_tsv
    ON proxima_core.agent_derivation_v1 USING gin (search_tsv);

-- A GIN index built over existing rows leaves its statistics unset, and the
-- planner prices an unanalysed GIN scan badly enough to keep choosing the
-- sequential path it was built to replace.
ANALYZE proxima_core.memories;
ANALYZE proxima_core.agent_note_v1;
ANALYZE proxima_core.agent_derivation_v1;
