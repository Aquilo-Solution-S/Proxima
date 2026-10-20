-- The capacity check of a listenable write read up to `max_pending` index
-- entries of the outbox backlog on every write. The publisher already walks
-- that backlog, so it keeps an estimate of its size here and a write reads
-- one row. One row, written by the publisher's claim at most every few
-- seconds, never by a write. `measured_at` starts at -infinity, so a write
-- that finds no fresh estimate falls back to the exact probe.
SELECT set_config('app.proxima_scope', 'platform', true);

CREATE TABLE proxima_core.publication_backlog (
    singleton boolean PRIMARY KEY DEFAULT true
        CONSTRAINT publication_backlog_singleton_chk CHECK (singleton),
    pending bigint NOT NULL
        CONSTRAINT publication_backlog_pending_chk CHECK (pending >= 0),
    measured_at timestamptz NOT NULL
);

INSERT INTO proxima_core.publication_backlog (singleton, pending, measured_at)
VALUES (true, 0, '-infinity');

COMMENT ON TABLE proxima_core.publication_backlog IS 'An estimate of how many outbox records are not yet published, and when the publisher measured it. A listenable write trusts a fresh estimate below the pending ceiling instead of probing the backlog; an old one sends it to the exact probe.';
