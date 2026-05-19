ALTER TABLE proxima_core.personality_wake_invocation_logs
    DROP CONSTRAINT personality_wake_invocation_l_owner_principal_kind_owner_p_fkey;

ALTER TABLE proxima_core.personality_wake_invocations
    DROP CONSTRAINT personality_wake_invocations_pkey;

ALTER TABLE proxima_core.personality_wake_invocations
    ADD COLUMN invocation_id uuid,
    ADD COLUMN continuation_budget_decision_memory_id uuid,
    ADD COLUMN continuation_original_invocation_id uuid;

UPDATE proxima_core.personality_wake_invocations
   SET invocation_id = COALESCE(NULLIF(wake_token, '00000000-0000-0000-0000-000000000000'::uuid), change_event_seq);

ALTER TABLE proxima_core.personality_wake_invocations
    ALTER COLUMN invocation_id SET NOT NULL,
    ADD CONSTRAINT personality_wake_invocations_pkey PRIMARY KEY (invocation_id),
    ADD CONSTRAINT personality_wake_invocations_continuation_decision_fkey
        FOREIGN KEY (continuation_budget_decision_memory_id)
        REFERENCES proxima_core.memories(memory_id);

CREATE UNIQUE INDEX personality_wake_invocations_normal_uq
    ON proxima_core.personality_wake_invocations
        (owner_principal_kind, owner_principal_id, owner_org_id,
         personality_instance_id, wake_entry_id, change_event_seq)
    WHERE continuation_budget_decision_memory_id IS NULL;

CREATE UNIQUE INDEX personality_wake_invocations_continuation_decision_uq
    ON proxima_core.personality_wake_invocations (continuation_budget_decision_memory_id)
    WHERE continuation_budget_decision_memory_id IS NOT NULL;

ALTER TABLE proxima_core.personality_wake_invocation_logs
    ADD COLUMN invocation_id uuid;

UPDATE proxima_core.personality_wake_invocation_logs l
   SET invocation_id = i.invocation_id
  FROM proxima_core.personality_wake_invocations i
 WHERE l.owner_principal_kind = i.owner_principal_kind
   AND l.owner_principal_id = i.owner_principal_id
   AND l.owner_org_id = i.owner_org_id
   AND l.personality_instance_id = i.personality_instance_id
   AND l.wake_entry_id = i.wake_entry_id
   AND l.change_event_seq = i.change_event_seq;

ALTER TABLE proxima_core.personality_wake_invocation_logs
    ALTER COLUMN invocation_id SET NOT NULL,
    ADD CONSTRAINT personality_wake_invocation_logs_invocation_fkey
        FOREIGN KEY (invocation_id)
        REFERENCES proxima_core.personality_wake_invocations(invocation_id)
        ON DELETE CASCADE;

CREATE INDEX personality_wake_invocation_logs_invocation_id_idx
    ON proxima_core.personality_wake_invocation_logs (invocation_id, log_seq);
