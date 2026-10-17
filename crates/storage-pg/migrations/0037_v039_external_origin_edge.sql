-- v0.0.39: an other-store row records the kind of its edge (issue #480).
--
--   memory_external_origin.edge_kind   'origin' or 'reference'
--
-- core_derive records another store's sources as origins; core_interpret
-- records another store's subjects, which are references (a local subject
-- of an interpretation is a Reference pin, never an origin). Until now both
-- landed as bare `(t, memory)` rows and every reader called them origins.
--
-- Backfill of the rows written before this column:
--   * hot parent (`memory`): `reference` when its schema is
--     `core/interpretation-v1`, otherwise `origin`;
--   * cooled parent (`cooled` has no schema column): `reference` when it is a
--     Perspective with no origins and at least one reference. That is the
--     shape interpret writes (it requires one local subject, writes it as a
--     reference and writes no origins); core_derive requires one local
--     source and always writes origins. Otherwise `origin`;
--   * a row with neither parent: `origin`.
--
-- Additive and idempotent: IF NOT EXISTS, a backfill of NULLs only, and a
-- guarded constraint. The primary key stays (t, memory). No policy change:
-- the owner policies are row-level.

SELECT set_config('app.proxima_scope', 'platform', true);

ALTER TABLE proxima_core.memory_external_origin
    ADD COLUMN IF NOT EXISTS edge_kind text;

UPDATE proxima_core.memory_external_origin e
   SET edge_kind = CASE
           WHEN m.schema_id = 'core/interpretation-v1' THEN 'reference'
           ELSE 'origin'
       END
  FROM proxima_core.memory m
 WHERE e.edge_kind IS NULL AND m.t = e.t;

UPDATE proxima_core.memory_external_origin e
   SET edge_kind = CASE
           WHEN c.kind = 'perspective'
            AND cardinality(coalesce(c.origins, '{}'::uuid[])) = 0
            AND cardinality(coalesce(c.refs, '{}'::uuid[])) > 0 THEN 'reference'
           ELSE 'origin'
       END
  FROM proxima_core.cooled c
 WHERE e.edge_kind IS NULL AND c.t = e.t;

UPDATE proxima_core.memory_external_origin
   SET edge_kind = 'origin'
 WHERE edge_kind IS NULL;

ALTER TABLE proxima_core.memory_external_origin
    ALTER COLUMN edge_kind SET NOT NULL;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conname = 'memory_external_origin_edge_kind_chk'
           AND conrelid = 'proxima_core.memory_external_origin'::regclass
    ) THEN
        ALTER TABLE proxima_core.memory_external_origin
            ADD CONSTRAINT memory_external_origin_edge_kind_chk
            CHECK (edge_kind IN ('origin', 'reference'));
    END IF;
END
$$;
