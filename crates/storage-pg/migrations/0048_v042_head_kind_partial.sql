-- A kind-filtered head page reads one owner's heads of one kind, newest first.
-- Under owner RLS the enum `=` on `kind` is not leakproof, so the planner cannot
-- use `kind` as an index condition and an owner with no heads of the requested
-- kind used to walk every head it has. One partial index per kind carries the
-- kind in its predicate instead; the page SQL writes the kind as a literal so
-- the predicate is provable in generic plans too. Each head lands in exactly
-- one of the three. `kind` is INCLUDEd although the predicate fixes it: the
-- memory_head read policy evaluates `kind`, and without it in the index the
-- scan cannot be index-only and loses to `memory_head_owner_t_idx` (which
-- carries `kind`) on cost. `memory_head_owner_kind_idx` stays (per-kind counts).
CREATE INDEX memory_head_owner_fact_t_idx ON proxima_core.memory_head USING btree (owner_id, t DESC) INCLUDE (handle, kind) WHERE kind = 'fact'::proxima_core.memory_kind;
CREATE INDEX memory_head_owner_abstraction_t_idx ON proxima_core.memory_head USING btree (owner_id, t DESC) INCLUDE (handle, kind) WHERE kind = 'abstraction'::proxima_core.memory_kind;
CREATE INDEX memory_head_owner_perspective_t_idx ON proxima_core.memory_head USING btree (owner_id, t DESC) INCLUDE (handle, kind) WHERE kind = 'perspective'::proxima_core.memory_kind;
