-- S0 — Owner = Principal collapse (Track B): drop owner_org_id from proxima_core.
--
-- DDL-drop strategy (no data re-write): the live brain is single-org, so
-- dropping owner_org_id from the composite natural keys cannot create a
-- collision (the column is constant across all rows; the Step-0 single-org
-- guard proves it). Existing rows survive untouched; identity hashes/keys and
-- S3 object keys are opaque/stored and are NOT recomputed. Only future writes
-- go org-free.
--
-- Order: composite FKs → composite PK/UNIQUE constraints → owner indexes →
-- recreate shrunk principal-only keys → rewrite edge-invariant trigger →
-- drop the owner_org_id column from all 18 core tables.
--
-- All objects are schema-qualified; no `SET search_path` (it would persist on
-- the pooled migration connection and leak `proxima_core` into the search_path
-- of later queries, breaking `to_regclass(...)::text` schema-qualification).

-- 1. Composite FKs first (they reference the personality composite key).
ALTER TABLE proxima_core.personality_wake_entries
    DROP CONSTRAINT personality_wake_entries_owner_principal_kind_owner_princi_fkey;
ALTER TABLE proxima_core.read_scope_matrix
    DROP CONSTRAINT read_scope_matrix_owner_principal_kind_owner_principal_id__fkey;
ALTER TABLE proxima_core.read_scope_matrix
    DROP CONSTRAINT read_scope_matrix_owner_principal_kind_owner_principal_id_fkey1;

-- 2. PK/UNIQUE constraints embedding owner_org_id (13).
ALTER TABLE proxima_core.cited_object_uploads
    DROP CONSTRAINT cited_object_uploads_pkey;
ALTER TABLE proxima_core.cited_objects
    DROP CONSTRAINT cited_objects_unique_per_owner;
ALTER TABLE proxima_core.embedding_jobs
    DROP CONSTRAINT embedding_jobs_pkey;
ALTER TABLE proxima_core.fact_entities
    DROP CONSTRAINT fact_entities_identity_uq;
ALTER TABLE proxima_core.goals
    DROP CONSTRAINT goals_request_id_idem;
ALTER TABLE proxima_core.master_token_personality
    DROP CONSTRAINT master_token_personality_pkey;
ALTER TABLE proxima_core.owner_fact_retention
    DROP CONSTRAINT owner_fact_retention_pkey;
ALTER TABLE proxima_core.personality
    DROP CONSTRAINT personality_pkey;
ALTER TABLE proxima_core.personality_wake_entries
    DROP CONSTRAINT personality_wake_entries_pkey;
ALTER TABLE proxima_core.read_scope_matrix
    DROP CONSTRAINT read_scope_matrix_pkey;
ALTER TABLE proxima_core.source_batches
    DROP CONSTRAINT source_batches_unique_per_source;
ALTER TABLE proxima_core.subject_personality
    DROP CONSTRAINT subject_personality_pkey;

-- 3. Owner indexes (9). personality_wake_entries_active_trigger_uq is a unique
--    index (not a constraint), so it is dropped with the other indexes.
DROP INDEX proxima_core.idx_edges_owner;
DROP INDEX proxima_core.idx_embeddings_owner;
DROP INDEX proxima_core.idx_events_owner_observed;
DROP INDEX proxima_core.idx_goals_owner_state;
DROP INDEX proxima_core.idx_memories_owner_kind;
DROP INDEX proxima_core.idx_memories_retention_due;
DROP INDEX proxima_core.idx_read_scope_matrix_readable;
DROP INDEX proxima_core.idx_source_batches_owner;
DROP INDEX proxima_core.personality_wake_entries_active_trigger_uq;

-- 4. Recreate the shrunk natural keys (principal-only).
ALTER TABLE proxima_core.cited_object_uploads
    ADD CONSTRAINT cited_object_uploads_pkey
    PRIMARY KEY (owner_principal_kind, owner_principal_id, upload_id);
ALTER TABLE proxima_core.cited_objects
    ADD CONSTRAINT cited_objects_unique_per_owner
    UNIQUE (owner_principal_kind, owner_principal_id, schema_id, content_hash);
ALTER TABLE proxima_core.embedding_jobs
    ADD CONSTRAINT embedding_jobs_pkey
    PRIMARY KEY (owner_principal_kind, owner_principal_id, entity_kind, entity_id, model_id, embedding_version);
ALTER TABLE proxima_core.fact_entities
    ADD CONSTRAINT fact_entities_identity_uq
    UNIQUE (owner_principal_kind, owner_principal_id, schema_id, schema_version, natural_key);
ALTER TABLE proxima_core.goals
    ADD CONSTRAINT goals_request_id_idem
    UNIQUE (owner_principal_kind, owner_principal_id, request_id);
ALTER TABLE proxima_core.master_token_personality
    ADD CONSTRAINT master_token_personality_pkey
    PRIMARY KEY (master_token_id, owner_principal_kind, owner_principal_id);
ALTER TABLE proxima_core.owner_fact_retention
    ADD CONSTRAINT owner_fact_retention_pkey
    PRIMARY KEY (owner_principal_kind, owner_principal_id);
ALTER TABLE proxima_core.personality
    ADD CONSTRAINT personality_pkey
    PRIMARY KEY (owner_principal_kind, owner_principal_id, personality_instance_id);
ALTER TABLE proxima_core.personality_wake_entries
    ADD CONSTRAINT personality_wake_entries_pkey
    PRIMARY KEY (owner_principal_kind, owner_principal_id, personality_instance_id, wake_entry_id);
ALTER TABLE proxima_core.read_scope_matrix
    ADD CONSTRAINT read_scope_matrix_pkey
    PRIMARY KEY (owner_principal_kind, owner_principal_id, reader_personality_instance_id, readable_personality_instance_id);
ALTER TABLE proxima_core.source_batches
    ADD CONSTRAINT source_batches_unique_per_source
    UNIQUE (source_id, owner_principal_kind, owner_principal_id, id);
ALTER TABLE proxima_core.subject_personality
    ADD CONSTRAINT subject_personality_pkey
    PRIMARY KEY (subject_principal_kind, subject_principal_id, owner_principal_kind, owner_principal_id);

CREATE UNIQUE INDEX personality_wake_entries_active_trigger_uq
    ON proxima_core.personality_wake_entries
    USING btree (owner_principal_kind, owner_principal_id, personality_instance_id, trigger_kind, trigger_id)
    WHERE (tombstoned_at IS NULL);

-- Recreate the composite FKs against the shrunk personality key.
ALTER TABLE proxima_core.personality_wake_entries
    ADD CONSTRAINT personality_wake_entries_owner_principal_kind_owner_princi_fkey
    FOREIGN KEY (owner_principal_kind, owner_principal_id, personality_instance_id)
    REFERENCES proxima_core.personality(owner_principal_kind, owner_principal_id, personality_instance_id);
ALTER TABLE proxima_core.read_scope_matrix
    ADD CONSTRAINT read_scope_matrix_owner_principal_kind_owner_principal_id__fkey
    FOREIGN KEY (owner_principal_kind, owner_principal_id, reader_personality_instance_id)
    REFERENCES proxima_core.personality(owner_principal_kind, owner_principal_id, personality_instance_id);
ALTER TABLE proxima_core.read_scope_matrix
    ADD CONSTRAINT read_scope_matrix_owner_principal_kind_owner_principal_id_fkey1
    FOREIGN KEY (owner_principal_kind, owner_principal_id, readable_personality_instance_id)
    REFERENCES proxima_core.personality(owner_principal_kind, owner_principal_id, personality_instance_id);

-- 5. Re-create owner indexes (principal-only).
CREATE INDEX idx_edges_owner
    ON proxima_core.edges USING btree (owner_principal_kind, owner_principal_id);
CREATE INDEX idx_embeddings_owner
    ON proxima_core.embeddings USING btree (owner_principal_kind, owner_principal_id);
CREATE INDEX idx_events_owner_observed
    ON proxima_core.events USING btree (owner_principal_kind, owner_principal_id, observed_at DESC);
CREATE INDEX idx_goals_owner_state
    ON proxima_core.goals USING btree (owner_principal_kind, owner_principal_id, state);
CREATE INDEX idx_memories_owner_kind
    ON proxima_core.memories USING btree (owner_principal_kind, owner_principal_id, kind);
CREATE INDEX idx_memories_retention_due
    ON proxima_core.memories USING btree (owner_principal_kind, owner_principal_id, created_at)
    WHERE ((event_id IS NOT NULL) AND (citation_mapping_id IS NOT NULL) AND (tombstoned_at IS NULL));
CREATE INDEX idx_read_scope_matrix_readable
    ON proxima_core.read_scope_matrix USING btree (owner_principal_kind, owner_principal_id, readable_personality_instance_id);
CREATE INDEX idx_source_batches_owner
    ON proxima_core.source_batches USING btree (owner_principal_kind, owner_principal_id);

-- 6. Rewrite validate_edge_invariants(): drop the three org variables and the
--    two org comparisons; compare principals only.
CREATE OR REPLACE FUNCTION proxima_core.validate_edge_invariants() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    source_actual_kind proxima_core.entity_kind;
    source_owner_kind proxima_core.owner_principal_kind;
    source_owner_id uuid;
    target_actual_kind proxima_core.entity_kind;
    target_owner_kind proxima_core.owner_principal_kind;
    target_owner_id uuid;
    source_layer int;
    target_layer int;
BEGIN
    IF NEW.source_memory_id IS NOT NULL THEN
        SELECT proxima_core.memory_entity_kind(kind),
               owner_principal_kind,
               owner_principal_id
          INTO source_actual_kind,
               source_owner_kind,
               source_owner_id
         FROM proxima_core.memories
         WHERE memory_id = NEW.source_memory_id;
    ELSIF NEW.source_goal_id IS NOT NULL THEN
        SELECT 'Goal'::proxima_core.entity_kind,
               owner_principal_kind,
               owner_principal_id
          INTO source_actual_kind,
               source_owner_kind,
               source_owner_id
          FROM proxima_core.goals
         WHERE goal_id = NEW.source_goal_id;
    ELSE
        SELECT 'Fact'::proxima_core.entity_kind,
               owner_principal_kind,
               owner_principal_id
          INTO source_actual_kind,
               source_owner_kind,
               source_owner_id
          FROM proxima_core.fact_entities
         WHERE fact_entity_id = NEW.source_fact_entity_id;
    END IF;

    IF NEW.target_memory_id IS NOT NULL THEN
        SELECT proxima_core.memory_entity_kind(kind),
               owner_principal_kind,
               owner_principal_id
          INTO target_actual_kind,
               target_owner_kind,
               target_owner_id
         FROM proxima_core.memories
         WHERE memory_id = NEW.target_memory_id;
    ELSIF NEW.target_goal_id IS NOT NULL THEN
        SELECT 'Goal'::proxima_core.entity_kind,
               owner_principal_kind,
               owner_principal_id
          INTO target_actual_kind,
               target_owner_kind,
               target_owner_id
          FROM proxima_core.goals
         WHERE goal_id = NEW.target_goal_id;
    ELSE
        SELECT 'Fact'::proxima_core.entity_kind,
               owner_principal_kind,
               owner_principal_id
          INTO target_actual_kind,
               target_owner_kind,
               target_owner_id
          FROM proxima_core.fact_entities
         WHERE fact_entity_id = NEW.target_fact_entity_id;
    END IF;

    IF source_actual_kind IS NULL THEN
        RAISE EXCEPTION 'edge: source endpoint not found';
    END IF;
    IF target_actual_kind IS NULL THEN
        RAISE EXCEPTION 'edge: target endpoint not found';
    END IF;
    IF NEW.source_kind <> source_actual_kind THEN
        RAISE EXCEPTION 'edge: source kind % does not match endpoint kind %',
            NEW.source_kind, source_actual_kind;
    END IF;
    IF NEW.target_kind <> target_actual_kind THEN
        RAISE EXCEPTION 'edge: target kind % does not match endpoint kind %',
            NEW.target_kind, target_actual_kind;
    END IF;

    IF source_owner_kind <> NEW.owner_principal_kind
       OR source_owner_id <> NEW.owner_principal_id THEN
        RAISE EXCEPTION 'edge: source crosses Owner boundary';
    END IF;
    IF target_owner_kind <> NEW.owner_principal_kind
       OR target_owner_id <> NEW.owner_principal_id THEN
        RAISE EXCEPTION 'edge: target crosses Owner boundary';
    END IF;

    source_layer := proxima_core.edge_layer(NEW.source_kind);
    target_layer := proxima_core.edge_layer(NEW.target_kind);
    IF source_layer IS NOT NULL
       AND target_layer IS NOT NULL
       AND source_layer < target_layer THEN
        RAISE EXCEPTION 'edge: F/A/P layer violation % -> %',
            NEW.source_kind, NEW.target_kind;
    END IF;

    IF NEW.source_kind = 'Fact'
       AND NEW.target_kind = 'Fact'
       AND NEW.relation_class IN ('Causal', 'Interpretive') THEN
        RAISE EXCEPTION 'edge: semantic Fact-to-Fact edges are forbidden';
    END IF;

    IF NEW.relation_class = 'Supersession' THEN
        IF NEW.source_kind = 'Fact' OR NEW.target_kind = 'Fact' THEN
            RAISE EXCEPTION 'edge: Facts cannot be superseded';
        END IF;
        IF NEW.source_kind <> NEW.target_kind THEN
            RAISE EXCEPTION 'edge: supersession requires matching endpoint kinds';
        END IF;
    END IF;

    RETURN NEW;
END;
$$;

-- 7. Drop the owner_org_id column from all 18 core tables.
ALTER TABLE proxima_core.change_event DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.citation_mappings DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.cited_object_uploads DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.cited_objects DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.edges DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.embeddings DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.embedding_jobs DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.events DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.fact_entities DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.goals DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.master_token_personality DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.memories DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.owner_fact_retention DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.personality DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.personality_wake_entries DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.read_scope_matrix DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.source_batches DROP COLUMN owner_org_id;
ALTER TABLE proxima_core.subject_personality DROP COLUMN owner_org_id;
