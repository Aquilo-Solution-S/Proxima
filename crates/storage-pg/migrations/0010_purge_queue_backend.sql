-- Backend identity on the durable object-purge queue.
--
-- `cold_purge_pending` is the erase's outbox: a row means "these bytes still
-- exist and the erase that promised to reclaim them has not". It named the
-- object key and nothing else, so which object STORE holds those bytes was
-- boot convention — the wired `ColdObjectStore` was assumed to be the one the
-- rows were enqueued against. A deployment pointed at a second bucket drains
-- the queue against the wrong store: every delete is a no-op there, the debt
-- is cleared, and the bytes the erase promised to destroy survive.
--
-- `backend` records it. `blob_uploads.bucket` is the authority for a cited
-- upload object, because that is the bucket the object was published to.
ALTER TABLE proxima_core.cold_purge_pending
    ADD COLUMN backend text;

-- Backfill, in the one direction the data supports: an upload row that still
-- names the queued key knows its bucket. Rows with no such row are cold Memory
-- objects (one store per deployment, by construction) or pre-0010 debts, and
-- both mean "whatever store is wired" — the empty string, which
-- `proxima_core::UNRECORDED_BACKEND` names and the drain adopts.
UPDATE proxima_core.cold_purge_pending p
   SET backend = u.bucket
  FROM proxima_core.blob_uploads u
 WHERE u.object_key = p.object_key
   AND p.backend IS NULL;

UPDATE proxima_core.cold_purge_pending
   SET backend = ''
 WHERE backend IS NULL;

ALTER TABLE proxima_core.cold_purge_pending
    ALTER COLUMN backend SET NOT NULL,
    ALTER COLUMN backend SET DEFAULT '';

-- An orphan enqueued by a stage whose owner row was erased underneath it has
-- no owner left to reference: the FK's target row is gone by the time the
-- orphan is discovered. The column stays (an operator wants to know whose
-- erase owes the bytes when the owner survives) and the FK stays (a non-NULL
-- value must still resolve); only the NOT NULL goes, because "no surviving
-- owner" is a state the queue has to be able to represent rather than a
-- reason to drop the debt.
ALTER TABLE proxima_core.cold_purge_pending
    ALTER COLUMN owner_id DROP NOT NULL;

-- The drain reads by backend before it reads by age.
CREATE INDEX cold_purge_pending_backend_idx
    ON proxima_core.cold_purge_pending (backend, enqueued_at, object_key);
