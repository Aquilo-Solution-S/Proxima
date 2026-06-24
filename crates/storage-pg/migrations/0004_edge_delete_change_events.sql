ALTER TYPE proxima_core.change_event_kind ADD VALUE IF NOT EXISTS 'EdgeDelete';

ALTER TABLE proxima_core.change_event
    DROP CONSTRAINT change_event_endpoint_chk;

ALTER TABLE proxima_core.change_event
    ADD CONSTRAINT change_event_endpoint_chk CHECK (
        CASE
            WHEN kind::text IN ('EdgeAppend', 'EdgeDelete') THEN
                entity_kind IS NULL
                AND entity_memory_id IS NULL AND entity_goal_id IS NULL
                AND entity_schema_id IS NULL AND entity_schema_version IS NULL
                AND supersedes_memory_id IS NULL AND supersedes_goal_id IS NULL
                AND edge_id IS NOT NULL AND edge_relation IS NOT NULL
                AND num_nonnulls(edge_source_memory_id, edge_source_goal_id, edge_source_fact_entity_id) = 1
                AND num_nonnulls(edge_target_memory_id, edge_target_goal_id, edge_target_fact_entity_id) = 1
            ELSE
                num_nonnulls(entity_memory_id, entity_goal_id) = 1
                AND entity_kind IS NOT NULL
                AND entity_schema_id IS NOT NULL
                AND entity_schema_version IS NOT NULL
                AND edge_id IS NULL AND edge_relation IS NULL
                AND edge_source_memory_id IS NULL AND edge_source_goal_id IS NULL AND edge_source_fact_entity_id IS NULL
                AND edge_target_memory_id IS NULL AND edge_target_goal_id IS NULL AND edge_target_fact_entity_id IS NULL
                AND NOT (supersedes_memory_id IS NOT NULL AND supersedes_goal_id IS NOT NULL)
        END
    );

COMMENT ON CONSTRAINT change_event_endpoint_chk ON proxima_core.change_event IS
  'Endpoint XOR + not-null companions guarding the pull-read decode (change_event.rs). EdgeAppend/EdgeDelete rows carry edge_id/edge_relation and exactly one of *_memory_id/*_goal_id/*_fact_entity_id per edge endpoint, with all entity/supersedes columns NULL. EntityAppend/EntityDelete rows carry exactly one of entity_memory_id/entity_goal_id plus entity_kind/schema, at most one supersedes endpoint, and all edge columns NULL. Mirrors edges_source/target_endpoint_chk; keeps a raw INSERT from persisting an undecodable row.';
