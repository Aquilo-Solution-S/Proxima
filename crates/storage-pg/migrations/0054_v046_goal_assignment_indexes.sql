-- Recall reads the Active goals assigned to a Perspective, and the Active
-- goals assigned to none. Neither was served by an index: the assigned query
-- scanned the goal table, and the unassigned one paged every Active goal of the
-- owner. The first index holds the goals of one Perspective; the second only
-- the unassigned ones, in the order the goal list reads them.
CREATE INDEX goal_assignment_state_t_idx
    ON proxima_core.goal USING btree (assignment_t, state, t DESC)
    WHERE assignment_t IS NOT NULL;
CREATE INDEX goal_unassigned_owner_state_idx
    ON proxima_core.goal USING btree (owner_id, state, handle DESC, t DESC)
    WHERE assignment_t IS NULL;
