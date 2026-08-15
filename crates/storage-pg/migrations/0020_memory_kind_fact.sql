-- Proxima core schema — v0.0.8 draft: store Fact as entity_kind 'Fact'.
--
-- 0001_init.sql encoded Fact as memories.kind IS NULL and forbade the
-- 'Fact' enum label on the column. Kernel and Rust already use
-- EntityKind::Fact. This migration closes the second encoding.
--
-- 0001_init.sql is the SHIPPED v0.0.4 baseline (sqlx checksum-pinned, NEVER
-- edit). This file is a v0.0.8-cycle DRAFT (docs/how-to/migrations.md).

ALTER TABLE proxima_core.memories
    DROP CONSTRAINT memories_kind_values_chk;

ALTER TABLE proxima_core.memories
    DROP CONSTRAINT memories_variant_chk;

ALTER TABLE proxima_core.memories
    DROP CONSTRAINT memories_fact_entity_chk;

ALTER TABLE proxima_core.memories
    DROP CONSTRAINT memories_superseded_by_not_a_fact_chk;

UPDATE proxima_core.memories
   SET kind = 'Fact'
 WHERE kind IS NULL;

ALTER TABLE proxima_core.memories
    ALTER COLUMN kind SET DEFAULT 'Fact';

ALTER TABLE proxima_core.memories
    ALTER COLUMN kind SET NOT NULL;

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_kind_values_chk CHECK (
        kind = ANY (ARRAY[
            'Fact'::proxima_core.entity_kind,
            'Abstraction'::proxima_core.entity_kind,
            'Perspective'::proxima_core.entity_kind
        ])
    );

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_variant_chk CHECK (
        (kind = 'Fact'::proxima_core.entity_kind
         AND operator_kind IS NULL AND operator_id IS NULL
         AND input_contract_id IS NULL AND source_batch_id IS NULL
         AND model_id IS NULL AND prompt_version IS NULL AND supersedes IS NULL)
        OR (kind <> 'Fact'::proxima_core.entity_kind
            AND text IS NOT NULL
            AND operator_kind IS NOT NULL
            AND operator_id IS NOT NULL
            AND input_contract_id IS NOT NULL
            AND (
                (operator_kind = 'FtoA'::proxima_core.memory_operator_kind
                 AND kind = 'Abstraction'::proxima_core.entity_kind
                 AND source_batch_id IS NOT NULL)
                OR (operator_kind = 'AtoA'::proxima_core.memory_operator_kind
                    AND kind = 'Abstraction'::proxima_core.entity_kind
                    AND source_batch_id IS NULL)
                OR (operator_kind = 'AtoP'::proxima_core.memory_operator_kind
                    AND kind = 'Perspective'::proxima_core.entity_kind
                    AND source_batch_id IS NULL)
            )
            AND model_id IS NOT NULL
            AND prompt_version IS NOT NULL
            AND receipt_id IS NULL
            AND (citation_mapping_id IS NULL
                 OR kind = 'Abstraction'::proxima_core.entity_kind))
    );

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_fact_entity_chk
        CHECK (fact_entity_id IS NULL OR kind = 'Fact'::proxima_core.entity_kind);

ALTER TABLE proxima_core.memories
    ADD CONSTRAINT memories_superseded_by_not_a_fact_chk
        CHECK (superseded_by IS NULL OR kind <> 'Fact'::proxima_core.entity_kind);

COMMENT ON COLUMN proxima_core.memories.kind IS
  'Fact | Abstraction | Perspective. Stored explicitly; Goal is not a memory kind.';

DROP INDEX IF EXISTS proxima_core.idx_memories_owner_fact_created_id_live;
CREATE INDEX idx_memories_owner_fact_created_id_live
    ON proxima_core.memories USING btree
        (owner_kind, owner_id, created_at DESC, memory_id DESC)
    WHERE ((tombstoned_at IS NULL)
       AND (kind = 'Fact'::proxima_core.entity_kind));

DROP INDEX IF EXISTS proxima_core.idx_memories_retention_due;
CREATE INDEX idx_memories_retention_due
    ON proxima_core.memories USING btree
        (owner_kind, owner_id, created_at)
    WHERE ((kind = 'Fact'::proxima_core.entity_kind)
       AND (citation_mapping_id IS NOT NULL)
       AND (tombstoned_at IS NULL));

CREATE OR REPLACE FUNCTION proxima_core.edge_endpoint_row(
    endpoint_kind proxima_core.edge_endpoint_kind,
    endpoint_id uuid
)
RETURNS TABLE (
    actual_kind proxima_core.edge_endpoint_kind,
    owner_kind proxima_core.owner_ref_kind,
    owner_id uuid
)
    LANGUAGE sql STABLE
    AS $$
    SELECT m.kind::text::proxima_core.edge_endpoint_kind,
           m.owner_kind,
           m.owner_id
      FROM proxima_core.memories m
     WHERE endpoint_kind <> 'Goal'::proxima_core.edge_endpoint_kind
       AND endpoint_kind <> 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND m.memory_id = endpoint_id
    UNION ALL
    SELECT 'Goal'::proxima_core.edge_endpoint_kind, g.owner_kind, g.owner_id
      FROM proxima_core.goals g
     WHERE endpoint_kind = 'Goal'::proxima_core.edge_endpoint_kind
       AND g.goal_id = endpoint_id
    UNION ALL
    SELECT 'FactEntityHead'::proxima_core.edge_endpoint_kind, fe.owner_kind, fe.owner_id
      FROM proxima_core.fact_entities fe
     WHERE endpoint_kind = 'FactEntityHead'::proxima_core.edge_endpoint_kind
       AND fe.fact_entity_id = endpoint_id
$$;
