-- Exact Goal-command replay is a persisted declaration, not a renewed
-- admission check. The referenced Goal row is historical state and may
-- legitimately outlive assignment, evidence, or wake targets that were live
-- when it was authored.
CREATE TABLE proxima_core.goal_replay_declaration (
    goal_t uuid PRIMARY KEY
        REFERENCES proxima_core.goal (t) ON DELETE CASCADE,
    declaration jsonb NOT NULL,
    edge_count integer NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT goal_replay_declaration_object_chk CHECK (jsonb_typeof(declaration) = 'object'),
    CONSTRAINT goal_replay_edge_count_chk CHECK (edge_count >= 0)
);

CREATE TRIGGER goal_replay_declaration_append_only
    BEFORE UPDATE ON proxima_core.goal_replay_declaration
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

-- Existing Goals predate an exact declaration snapshot and are deliberately
-- not guessed from current target rows. Reusing one of their request ids
-- fails closed as an idempotency conflict; every new write records this row
-- in the same transaction as the Goal and its topology.
