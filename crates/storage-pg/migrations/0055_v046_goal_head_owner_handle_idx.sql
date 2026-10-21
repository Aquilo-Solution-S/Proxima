-- The goal list reads each owner's heads newest first and stops at the page.
-- The head table had no index that starts at the owner and ends in the handle.
CREATE INDEX goal_head_owner_handle_idx
    ON proxima_core.goal_head USING btree (owner_id, handle);
