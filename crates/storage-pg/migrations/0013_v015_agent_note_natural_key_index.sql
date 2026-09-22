-- Additive: index the one natural key the core flavor declares.
--
-- `core/agent-note-v1` is the only core contract with a non-empty
-- `natural_key_columns` (`note_id`). Admission resolves a natural key
-- through `owned_head_handle`, which filters the sidecar on that column:
--
--     SELECT h.handle FROM proxima_core.agent_note_v1 s
--       JOIN proxima_core.memory m ON m.t = s.t
--       JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
--      WHERE m.owner_id = $1 AND m.schema_id = $2 AND s.note_id = $3 LIMIT 1
--
-- The table shipped with only its primary key on `t`, so `note_id` was no
-- access path and the planner inverted the join: scan the owner's `memory`,
-- hash `memory_head`, then probe `agent_note_v1` by `t` once per surviving
-- row with `note_id` as a post-filter. Every note admission walked the
-- owner's whole memory table — O(rows) per write, and invisible while the
-- store is small: 1.2 ms and 1,301 shared buffer hits at 425 rows, under
-- any slow-query threshold that would report it.
--
-- The code flavor already ships this shape for its own natural key
-- (`idx_file_revision_v1_nk` on `(repo_id, file_path)`, v008 baseline).
-- Core never got one. Same name, same purpose.
--
-- Deliberately NOT unique. A natural key identifies a SERIES, and a series
-- is many admissions sharing one handle — each appending a row that repeats
-- `note_id`. A unique index would reject the second version of a note,
-- which is precisely what this lookup exists to find.

CREATE INDEX idx_agent_note_v1_nk
    ON proxima_core.agent_note_v1 USING btree (note_id);
