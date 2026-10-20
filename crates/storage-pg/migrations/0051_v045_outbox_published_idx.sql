-- The retention prune deletes delivered records oldest first. Without an index
-- on `published_at` every batch scanned the whole table; this one holds only
-- the delivered rows, so a prune batch reads the rows it deletes.
CREATE INDEX publication_outbox_published_idx
    ON proxima_core.publication_outbox USING btree (published_at)
    WHERE state = 'published'::proxima_core.publication_record_state;
