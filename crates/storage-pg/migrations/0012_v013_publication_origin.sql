-- Exact, payload-free publication provenance. The immutable identity here is
-- the owner/source that captured a published Fact, even after the Fact moves
-- or its hot payload is cooled/pruned.
CREATE TABLE proxima_core.publication_origin (
    t uuid PRIMARY KEY,
    original_owner_id uuid NOT NULL
        REFERENCES proxima_core.owners (owner_id),
    original_owner_kind proxima_core.owner_kind NOT NULL,
    source_id text,
    CONSTRAINT publication_origin_owner_kind_chk CHECK (
        original_owner_kind IN ('personal', 'group')
    )
);

CREATE INDEX publication_origin_owner_source_idx
    ON proxima_core.publication_origin
        (original_owner_kind, original_owner_id, source_id, t);

COMMENT ON TABLE proxima_core.publication_origin IS
'Payload-free immutable original publication identity. Lifecycle erasure may delete the row; payload and broker delivery state remain exclusively in publication_outbox.';

CREATE FUNCTION proxima_core.enforce_publication_origin_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'append-only: publication_origin identity cannot be updated'
        USING ERRCODE = '25006';
END;
$$;

CREATE TRIGGER publication_origin_identity_immutable
    BEFORE UPDATE ON proxima_core.publication_origin
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_publication_origin_immutable();

-- Backfill only where a retained hot/cooled Fact proves source identity.
-- Both NULL source fields mean known-none; one NULL is malformed, and no
-- retained Fact row means unknown. A historical hard-delete witness excludes
-- the identity even if an inconsistent outbox row remains.
WITH retained_facts AS (
    SELECT m.t, m.owner_id, m.kind, m.source_id, m.ingest_key
      FROM proxima_core.memory m
     WHERE m.kind = 'fact'
    UNION ALL
    SELECT c.t, c.owner_id, c.kind, c.source_id, c.ingest_key
      FROM proxima_core.cooled c
     WHERE c.kind = 'fact'
)
INSERT INTO proxima_core.publication_origin
    (t, original_owner_id, original_owner_kind, source_id)
SELECT o.t, o.owner_id, owner.kind, retained.source_id
  FROM proxima_core.publication_outbox o
  JOIN proxima_core.owners owner ON owner.owner_id = o.owner_id
  JOIN retained_facts retained
    ON retained.t = o.t
 WHERE retained.kind = 'fact'
   AND ((retained.source_id IS NULL AND retained.ingest_key IS NULL)
     OR (retained.source_id IS NOT NULL AND retained.ingest_key IS NOT NULL))
   AND NOT EXISTS (
       SELECT 1 FROM proxima_core.erased_pin_target erased
        WHERE erased.t = o.t
   );

-- This installation's own identity, minted once, here.
--
-- Publication-origin eligibility is answered from the ABSENCE of a row
-- above, and an absence carries no scope: "no origin row for this Fact"
-- reads identically whether erasure revoked it or the Fact was never in
-- this database. A retained-copy cleaner therefore cannot tell a revoked
-- copy from a copy some other installation published, and would read a
-- foreign stream as entirely revoked.
--
-- The publisher stamps this value on each broker message and the cleaner
-- compares it against what it reads HERE, from the same database that
-- answers the eligibility check, so the binding cannot be misconfigured
-- into agreement. A restore or clone carries the value with it; this
-- identifies an installation lineage, not a database instance.
CREATE TABLE proxima_core.installation (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    installation_id uuid NOT NULL DEFAULT gen_random_uuid()
);

COMMENT ON TABLE proxima_core.installation IS
'Payload-free deployment identity, minted at install and never updated. Stamped on published broker messages so retained-copy cleanup can prove a message is this installation''s before reading an absent origin row as a revocation.';

INSERT INTO proxima_core.installation (singleton) VALUES (true);

-- Rotating the identity would orphan every message already stamped with
-- the old one: the cleaner would read the whole stream as foreign and
-- refuse to clean it. Dropping it would leave the publisher unable to
-- stamp at all. The value is only ever minted, never changed or removed.
CREATE FUNCTION proxima_core.enforce_installation_immutable()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'write-once: the installation identity cannot be changed or removed'
        USING ERRCODE = '25006';
END;
$$;

CREATE TRIGGER installation_identity_immutable
    BEFORE UPDATE OR DELETE ON proxima_core.installation
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_installation_immutable();
