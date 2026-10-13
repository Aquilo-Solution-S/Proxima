-- v0.0.32: named outbox consumers, each with its own delivery lane, and a
-- per-series position on every captured event (docs/18 §Consumers,
-- §Ordering).
--
--   publication_consumer   one row per registered consumer. The runtime
--                          registers the consumers it is configured with at
--                          boot; only an explicit retire removes one. The
--                          single-intake API is the consumer `default`,
--                          seeded here, so a deployment that never names a
--                          consumer behaves as before.
--   publication_delivery   one lane per (record, consumer registered when the
--                          record was captured): its own state, claim, lease,
--                          attempts and published mark. A consumer that fails
--                          or lags holds only its own lanes.
--   publication_outbox     its state is now the AGGREGATE of its lanes:
--                          `pending` while any lane is undelivered,
--                          `published` (published_at = the last lane's mark)
--                          once none is, or at capture when no consumer was
--                          registered. The capacity probe, pending_count and
--                          retention keep reading it unchanged. A record is
--                          never `claimed` any more; claims live on lanes.
--                          Its claim and attempts columns stay (dropping them
--                          is not additive) and are always NULL / unused.
--   memory_head            publication_sequence: how many listenable events
--                          the series has captured. Capture increments it
--                          under the series lock and writes the new value
--                          into the event as `sequence`; it never decreases.
--
-- Backfill: every record not yet published becomes one `pending` lane of the
-- `default` consumer, keeping its attempts. A record a running publisher held
-- is handed back (its lease would have expired anyway): every older binary is
-- stopped before this file runs, and one that is not fails its next claim on
-- publication_outbox_no_claim_chk rather than delivering beside the lanes.
--
-- Owner RLS: publication_delivery is class fk_parent of its record (read in
-- owner scope through the record, written by the record's Fact owner — the
-- capture runs in the writer's owner scope). publication_consumer is
-- platform-written; owner scope may read it, because capture reads the
-- consumer set inside the writer's transaction.
--
-- Additive and idempotent: IF NOT EXISTS / OR REPLACE / ON CONFLICT
-- throughout, the backfill only inserts missing lanes, and the RLS appliers
-- replace the policies they make.

ALTER TABLE proxima_core.memory_head
    ADD COLUMN IF NOT EXISTS publication_sequence bigint NOT NULL DEFAULT 0
        CONSTRAINT memory_head_publication_sequence_chk CHECK (publication_sequence >= 0);

-- 0001's freeze, plus: the publication sequence never goes back, so no later
-- event of a series can reuse a position an earlier one was delivered with.
CREATE OR REPLACE FUNCTION proxima_core.memory_head_t_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id THEN
        RAISE EXCEPTION 'memory_head is frozen except t and owner_id'
            USING ERRCODE = '25006';
    END IF;
    IF NEW.publication_sequence < OLD.publication_sequence THEN
        RAISE EXCEPTION 'memory_head.publication_sequence never decreases'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TABLE IF NOT EXISTS proxima_core.publication_consumer (
    name text PRIMARY KEY
        CONSTRAINT publication_consumer_name_chk CHECK (name ~ '^[a-z0-9][a-z0-9_-]{0,62}$'),
    registered_at timestamptz NOT NULL DEFAULT now()
);

-- publication_consumer is platform-written (see below); the block binds the
-- platform scope itself so that a bare re-run of this file is a no-op rather
-- than a policy refusal, and restores the caller's scope afterwards.
DO $seed_default_consumer$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    INSERT INTO proxima_core.publication_consumer (name) VALUES ('default')
    ON CONFLICT (name) DO NOTHING;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
END
$seed_default_consumer$;

CREATE TABLE IF NOT EXISTS proxima_core.publication_delivery (
    t uuid NOT NULL REFERENCES proxima_core.publication_outbox (t) ON DELETE CASCADE,
    consumer text NOT NULL
        REFERENCES proxima_core.publication_consumer (name) ON DELETE CASCADE,
    state proxima_core.publication_state NOT NULL DEFAULT 'pending',
    claim_token uuid,
    claimed_by text,
    lease_expires_at timestamptz,
    attempts integer NOT NULL DEFAULT 0
        CONSTRAINT publication_delivery_attempts_chk CHECK (attempts >= 0),
    published_at timestamptz,
    PRIMARY KEY (t, consumer),
    CONSTRAINT publication_delivery_claim_chk CHECK (
        (state = 'claimed')
        = (claim_token IS NOT NULL
           AND lease_expires_at IS NOT NULL
           AND claimed_by IS NOT NULL)
    ),
    CONSTRAINT publication_delivery_published_chk CHECK (
        (state = 'published') = (published_at IS NOT NULL)
    )
);

-- A drain's only scan: one consumer's undelivered lanes, in claim order.
CREATE INDEX IF NOT EXISTS publication_delivery_pending_idx
    ON proxima_core.publication_delivery (consumer, attempts, t)
 WHERE state <> 'published';

-- Retire deletes by consumer; the cascade from a consumer needs this too.
CREATE INDEX IF NOT EXISTS publication_delivery_consumer_idx
    ON proxima_core.publication_delivery (consumer);

DO $backfill_default_lanes$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    INSERT INTO proxima_core.publication_delivery (t, consumer, attempts)
    SELECT p.t, 'default', p.attempts
      FROM proxima_core.publication_outbox AS p
     WHERE p.state <> 'published'
    ON CONFLICT (t, consumer) DO NOTHING;
    UPDATE proxima_core.publication_outbox
       SET state = 'pending',
           claim_token = NULL,
           claimed_by = NULL,
           lease_expires_at = NULL
     WHERE state = 'claimed';
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
END
$backfill_default_lanes$;

-- The record is an aggregate now; only its lanes are claimed.
ALTER TABLE proxima_core.publication_outbox
    DROP CONSTRAINT IF EXISTS publication_outbox_no_claim_chk;
ALTER TABLE proxima_core.publication_outbox
    ADD CONSTRAINT publication_outbox_no_claim_chk CHECK (state <> 'claimed');

-- Owner RLS through the per-table building blocks of 0024, as 0025 does. The
-- fixture lane that skips the owner-RLS migrations has neither the appliers
-- nor policies, so there is nothing to extend there.
DO $outbox_consumer_rls$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM pg_catalog.pg_proc AS p
          JOIN pg_catalog.pg_namespace AS n ON n.oid = p.pronamespace
         WHERE n.nspname = 'proxima_core' AND p.proname = 'owner_rls_apply_class'
    ) THEN
        PERFORM proxima_core.owner_rls_apply_class(
            '0026', 'class fk_parent', 'proxima_core', 'publication_delivery', 'fk_parent', NULL);
        PERFORM proxima_core.owner_rls_census_table('proxima_core', 'publication_delivery');
        -- ownerless (platform-only writes), with 0014's metadata read: any
        -- owner scope reads the consumer set its own capture fans out to.
        PERFORM proxima_core.owner_rls_apply_class(
            '0026', 'class ownerless', 'proxima_core', 'publication_consumer', 'ownerless', NULL);
        ALTER POLICY proxima_owner_read ON proxima_core.publication_consumer
            USING (current_setting('app.proxima_scope', true) = 'owner'
                   AND cardinality(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[])) > 0);
        PERFORM proxima_core.owner_rls_census_table('proxima_core', 'publication_consumer');
    END IF;
END
$outbox_consumer_rls$;
