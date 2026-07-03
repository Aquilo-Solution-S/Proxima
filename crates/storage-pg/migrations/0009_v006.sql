-- v0.0.6: lexical search no longer uses these core sidecar GIN indexes.
-- The storage search path builds a projection-owned candidates CTE and ranks
-- `to_tsvector('simple', c.index_text)` over that CTE, so these expression
-- indexes on the raw sidecar tables cannot be selected by the planner.
DROP INDEX IF EXISTS proxima_core.idx_agent_derivation_v1_search;
DROP INDEX IF EXISTS proxima_core.idx_agent_note_v1_search;
