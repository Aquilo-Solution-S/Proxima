-- #519: index debt. A store whose chunk vectors live in a host index keeps
-- each entity's head here and owes the index every later head change. A
-- statement trigger on embedding_heads writes the debt in the transaction that deletes
-- a head (forget, erase, space purge, the orphan sweep) or moves it to
-- another owner (transfer), so the debt survives a crash between that
-- commit and the index call. Maintenance applies each row to the host index
-- and deletes it; an erase applies its own before it commits. A store that
-- keeps its vectors locally clears the rows unapplied.
--
-- One row per key: a later change replaces an earlier one, and a new
-- version written by the drain deletes the key's row first.
SELECT set_config('app.proxima_scope', 'platform', true);

CREATE TABLE proxima_core.vector_index_debt (
    entity_id uuid NOT NULL,
    model_id text NOT NULL,
    dim smallint NOT NULL,
    -- The head's owner, already checked against owners where the head was
    -- written.
    owner_id uuid NOT NULL,
    op text NOT NULL,
    enqueued_at timestamptz NOT NULL DEFAULT now(),
    -- The top-level transaction that wrote the row, so an erase finds the
    -- debt it left, savepoints included.
    xact xid8 NOT NULL DEFAULT pg_current_xact_id(),
    PRIMARY KEY (entity_id, model_id, dim),
    CONSTRAINT vector_index_debt_op_chk CHECK (op IN ('delete', 'owner')),
    CONSTRAINT vector_index_debt_dim_lane_chk
        CHECK (dim = ANY (ARRAY[384, 768, 1024, 1536, 2048, 3072]))
);

COMMENT ON TABLE proxima_core.vector_index_debt IS 'What a store owes its host vector index: delete the key''s chunks, or record owner_id as their owner. Written by the embedding_heads trigger in the head change''s transaction; deleted once applied.';

ALTER TABLE proxima_core.vector_index_debt ENABLE ROW LEVEL SECURITY;
ALTER TABLE ONLY proxima_core.vector_index_debt FORCE ROW LEVEL SECURITY;
