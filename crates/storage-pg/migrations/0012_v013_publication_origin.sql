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
