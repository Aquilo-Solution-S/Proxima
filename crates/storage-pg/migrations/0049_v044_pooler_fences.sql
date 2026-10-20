-- #493: two coordination relations that hold no owner data, so that no
-- serving path needs a session-level advisory lock. A session lock outlives
-- its transaction on a shared server connection behind a transaction-mode
-- pooler, where another client's transaction inherits it.
--
--   export_erase_fence  Never holds a row; only its table lock is used.
--                       Owner export takes SHARE as the first statement of
--                       its REPEATABLE READ snapshot: LOCK TABLE takes no
--                       snapshot, so an export that waited behind an erase
--                       reads the erase's result. Erase takes ROW EXCLUSIVE.
--                       SHARE and ROW EXCLUSIVE conflict and each is
--                       self-compatible: exports run together, erases run
--                       together, an export and an erase never overlap.
--
--   maintenance_lease   One row per maintenance singleton (storage,
--                       embedding) while a pass holds it. A pass renews its
--                       lease between bounded transactions, and a lease that
--                       is not renewed expires. UNLOGGED: a lease is
--                       worthless after a crash, and the per-pass churn
--                       writes no WAL.
SELECT set_config('app.proxima_scope', 'platform', true);

CREATE TABLE proxima_core.export_erase_fence (
    singleton boolean PRIMARY KEY DEFAULT true
        CONSTRAINT export_erase_fence_empty_chk CHECK (false)
);

COMMENT ON TABLE proxima_core.export_erase_fence IS 'Never holds a row (CHECK false). Owner export locks it IN SHARE MODE before its snapshot and erase IN ROW EXCLUSIVE MODE, so exports and erases exclude each other without a session lock.';

CREATE UNLOGGED TABLE proxima_core.maintenance_lease (
    name text PRIMARY KEY
        CONSTRAINT maintenance_lease_name_chk CHECK (name IN ('storage', 'embedding')),
    holder uuid NOT NULL,
    expires_at timestamptz NOT NULL
);

COMMENT ON TABLE proxima_core.maintenance_lease IS 'The holder of each maintenance singleton (storage, embedding) and when its lease expires. A pass takes a missing or expired lease, renews its own, and deletes it when done; an unrenewed lease expires.';
