CREATE TABLE proxima_core.blocked_wake_candidates (
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    personality_instance_id uuid NOT NULL,
    wake_entry_id uuid NOT NULL,
    change_event_seq uuid NOT NULL,
    triggering_memory_id uuid NOT NULL,
    dependency_memory_id uuid NOT NULL,
    dependency_schema_id text NOT NULL,
    reason text NOT NULL,
    created_at timestamptz DEFAULT now() NOT NULL,
    updated_at timestamptz DEFAULT now() NOT NULL,
    CONSTRAINT blocked_wake_candidates_reason_chk CHECK (char_length(reason) >= 1),
    CONSTRAINT blocked_wake_candidates_dependency_schema_chk CHECK (char_length(dependency_schema_id) >= 1)
);

ALTER TABLE ONLY proxima_core.blocked_wake_candidates
    ADD CONSTRAINT blocked_wake_candidates_pkey
    PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id, wake_entry_id, change_event_seq);

ALTER TABLE ONLY proxima_core.blocked_wake_candidates
    ADD CONSTRAINT blocked_wake_candidates_wake_entry_fkey
    FOREIGN KEY (owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id, wake_entry_id)
    REFERENCES proxima_core.personality_wake_entries(owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id, wake_entry_id)
    ON DELETE CASCADE;

ALTER TABLE ONLY proxima_core.blocked_wake_candidates
    ADD CONSTRAINT blocked_wake_candidates_change_event_fkey
    FOREIGN KEY (change_event_seq)
    REFERENCES proxima_core.change_event(seq)
    ON DELETE CASCADE;

ALTER TABLE ONLY proxima_core.blocked_wake_candidates
    ADD CONSTRAINT blocked_wake_candidates_triggering_memory_fkey
    FOREIGN KEY (triggering_memory_id)
    REFERENCES proxima_core.memories(memory_id)
    ON DELETE CASCADE;

ALTER TABLE ONLY proxima_core.blocked_wake_candidates
    ADD CONSTRAINT blocked_wake_candidates_dependency_memory_fkey
    FOREIGN KEY (dependency_memory_id)
    REFERENCES proxima_core.memories(memory_id)
    ON DELETE CASCADE;

CREATE INDEX blocked_wake_candidates_scan_idx
    ON proxima_core.blocked_wake_candidates
    USING btree (owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id, updated_at);
