-- Transactional publication outbox for listenable Facts.
--
-- A Fact whose payload type declares `LISTENABLE = true` captures exactly one
-- CloudEvents 1.0 record in the SAME transaction as its `memory` row and its
-- typed sidecars. The record is keyed by the Fact's `t`, so the capture is
-- idempotent by primary key rather than by convention: a receipt replay
-- reuses `(handle, t)` and never reaches the insert, and a retried
-- transaction body re-inserts the same key.
--
-- Deliberately NO foreign key to `proxima_core.memory`. `forget` DELETEs the
-- hot `memory` row (it moves the payload to cold storage), so a cascading
-- reference would destroy a captured event that no publisher had delivered
-- yet — a committed event lost to an unrelated lifecycle operation. The
-- surface therefore declares `ForgetRule::Keep` and rests its completeness on
-- `owner_id -> owners`, exactly as `mcp_call_logged_v1` does for the same
-- reason. Single-memory erase deletes the row explicitly (`erase_memory`),
-- and owner erase reaches it through the surface's own `owner_id`.
DO $$
BEGIN
    IF to_regtype('proxima_core.publication_state') IS NULL THEN
        CREATE TYPE proxima_core.publication_state AS ENUM (
            'pending',
            'claimed',
            'published'
        );
    END IF;
END;
$$;

CREATE TABLE proxima_core.publication_outbox (
    t uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    schema_id text NOT NULL,
    schema_version integer NOT NULL,
    event_type text NOT NULL,
    event_id text NOT NULL UNIQUE,
    envelope bytea NOT NULL,
    envelope_digest bytea NOT NULL,
    state proxima_core.publication_state NOT NULL DEFAULT 'pending',
    claim_token uuid,
    claimed_by text,
    lease_expires_at timestamptz,
    attempts integer NOT NULL DEFAULT 0,
    published_at timestamptz,
    published_stream text,
    published_seq bigint,
    captured_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT publication_outbox_digest_len_chk
        CHECK (octet_length(envelope_digest) = 32),
    CONSTRAINT publication_outbox_claim_chk CHECK (
        (state = 'claimed')
        = (claim_token IS NOT NULL
           AND lease_expires_at IS NOT NULL
           AND claimed_by IS NOT NULL)
    ),
    CONSTRAINT publication_outbox_published_chk CHECK (
        (state = 'published')
        = (published_at IS NOT NULL
           AND published_seq IS NOT NULL
           AND published_stream IS NOT NULL)
    )
);

-- The drain's only scan. Partial, because a published record is never read
-- again: the index stays the size of the backlog, not of the history.
CREATE INDEX publication_outbox_pending_idx
    ON proxima_core.publication_outbox (t)
 WHERE state <> 'published';

CREATE INDEX publication_outbox_owner_idx
    ON proxima_core.publication_outbox (owner_id);

-- Append-only where it matters. The lifecycle columns (state, claim, lease,
-- attempts, publication receipt) are what the drain writes; the CAPTURED
-- event is immutable by constraint, not by convention, so no later
-- re-rendering, re-owning or re-keying can rewrite what a consumer will be
-- told happened.
CREATE FUNCTION proxima_core.enforce_publication_immutable() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.t IS DISTINCT FROM OLD.t
        OR NEW.event_id IS DISTINCT FROM OLD.event_id
        OR NEW.event_type IS DISTINCT FROM OLD.event_type
        OR NEW.envelope IS DISTINCT FROM OLD.envelope
        OR NEW.envelope_digest IS DISTINCT FROM OLD.envelope_digest
        OR NEW.owner_id IS DISTINCT FROM OLD.owner_id
        OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
        OR NEW.schema_version IS DISTINCT FROM OLD.schema_version
        OR NEW.captured_at IS DISTINCT FROM OLD.captured_at
    THEN
        RAISE EXCEPTION
            'append-only: publication_outbox does not accept UPDATE of the captured event'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER publication_outbox_capture_immutable
    BEFORE UPDATE ON proxima_core.publication_outbox
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_publication_immutable();
