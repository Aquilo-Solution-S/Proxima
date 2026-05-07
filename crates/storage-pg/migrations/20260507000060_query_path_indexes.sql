-- Query-path index cleanup: drop owner_org_id from the change_event
-- access predicate (AGENTS.md invariant 4 — org_id is not in the access
-- predicate; current callers in subscribe / event_history / event_ingest
-- never filter on it), and add btree indexes on FK child columns that
-- Postgres does not auto-index.

-- change_event ----------------------------------------------------------
DROP INDEX IF EXISTS proxima_core.idx_change_event_owner_seq;

CREATE INDEX idx_change_event_owner_seq
    ON proxima_core.change_event
       (owner_principal_kind, owner_principal_id, seq);

-- citation_mappings -----------------------------------------------------
CREATE INDEX idx_citation_mappings_memory_id
    ON proxima_core.citation_mappings (memory_id);

CREATE INDEX idx_citation_mappings_cited_object_id
    ON proxima_core.citation_mappings (cited_object_id);

-- goal_parents ----------------------------------------------------------
-- goal_id is already the leading column of the (goal_id, parent_goal_id)
-- primary key; only the inverse-traversal column needs its own index.
CREATE INDEX idx_goal_parents_parent_goal_id
    ON proxima_core.goal_parents (parent_goal_id);
