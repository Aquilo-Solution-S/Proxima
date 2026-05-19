ALTER TABLE proxima_core.personality_wake_entries
    RENAME COLUMN budgeter_personality_instance_id TO intervention_personality_instance_id;
ALTER TABLE proxima_core.personality_wake_entries
    RENAME COLUMN budget_extension_rounds TO intervention_extension_rounds;
ALTER TABLE proxima_core.personality_wake_entries
    RENAME COLUMN budget_hard_cap_rounds TO intervention_hard_cap_rounds;
ALTER TABLE proxima_core.personality_wake_entries
    RENAME COLUMN budget_progress_contract TO intervention_progress_contract;
ALTER TABLE proxima_core.personality_wake_entries
    RENAME CONSTRAINT personality_wake_entries_budget_rounds_chk
    TO personality_wake_entries_intervention_rounds_chk;
ALTER TABLE proxima_core.personality_wake_entries
    RENAME CONSTRAINT personality_wake_entries_budget_policy_chk
    TO personality_wake_entries_intervention_policy_chk;

ALTER TYPE proxima_core.budget_decision_kind
    RENAME TO intervention_decision_kind;

ALTER TABLE proxima_core.budget_review_requested_v1
    RENAME TO intervention_requested_v1;
ALTER TABLE proxima_core.intervention_requested_v1
    RENAME COLUMN target_budgeter_personality_instance_id
    TO target_intervention_personality_instance_id;
ALTER TABLE proxima_core.intervention_requested_v1
    RENAME COLUMN budget_extension_rounds TO intervention_extension_rounds;
ALTER TABLE proxima_core.intervention_requested_v1
    RENAME COLUMN budget_hard_cap_rounds TO intervention_hard_cap_rounds;
ALTER TABLE proxima_core.intervention_requested_v1
    RENAME CONSTRAINT budget_review_requested_v1_pkey
    TO intervention_requested_v1_pkey;
ALTER TABLE proxima_core.intervention_requested_v1
    RENAME CONSTRAINT budget_review_rounds_chk
    TO intervention_requested_rounds_chk;
ALTER TABLE proxima_core.intervention_requested_v1
    RENAME CONSTRAINT budget_review_progress_contract_chk
    TO intervention_requested_progress_contract_chk;
ALTER TABLE proxima_core.intervention_requested_v1
    RENAME CONSTRAINT budget_review_idempotency_key_chk
    TO intervention_requested_idempotency_key_chk;
ALTER INDEX proxima_core.budget_review_requested_invocation_uq
    RENAME TO intervention_requested_invocation_uq;
ALTER INDEX proxima_core.budget_review_requested_target_idx
    RENAME TO intervention_requested_target_idx;

ALTER TABLE proxima_core.budget_decision_v1
    RENAME TO intervention_decision_v1;
ALTER TABLE proxima_core.intervention_decision_v1
    RENAME COLUMN budget_request_memory_id TO intervention_request_memory_id;
ALTER TABLE proxima_core.intervention_decision_v1
    RENAME CONSTRAINT budget_decision_v1_pkey
    TO intervention_decision_v1_pkey;
ALTER TABLE proxima_core.intervention_decision_v1
    RENAME CONSTRAINT budget_decision_rounds_chk
    TO intervention_decision_rounds_chk;
ALTER TABLE proxima_core.intervention_decision_v1
    RENAME CONSTRAINT budget_decision_rationale_chk
    TO intervention_decision_rationale_chk;
ALTER TABLE proxima_core.intervention_decision_v1
    RENAME CONSTRAINT budget_decision_idempotency_key_chk
    TO intervention_decision_idempotency_key_chk;
ALTER INDEX proxima_core.budget_decision_idempotency_uq
    RENAME TO intervention_decision_idempotency_uq;
ALTER INDEX proxima_core.budget_decision_request_idx
    RENAME TO intervention_decision_request_idx;

ALTER TABLE proxima_core.personality_wake_invocations
    RENAME COLUMN continuation_budget_decision_memory_id
    TO continuation_intervention_decision_memory_id;
ALTER TABLE proxima_core.personality_wake_invocations
    RENAME CONSTRAINT personality_wake_invocations_continuation_decision_fkey
    TO personality_wake_invocations_continuation_intervention_decision_fkey;
ALTER INDEX proxima_core.personality_wake_invocations_continuation_decision_uq
    RENAME TO personality_wake_invocations_continuation_intervention_decision_uq;

UPDATE proxima_core.memories
   SET schema_id = CASE schema_id
       WHEN 'core/budget-review-requested-v1' THEN 'core/intervention-requested-v1'
       WHEN 'core/budget-decision-v1' THEN 'core/intervention-decision-v1'
       ELSE schema_id
   END
 WHERE schema_id IN ('core/budget-review-requested-v1', 'core/budget-decision-v1');

UPDATE proxima_core.events
   SET schema_id = CASE schema_id
       WHEN 'core/budget-review-requested-v1' THEN 'core/intervention-requested-v1'
       WHEN 'core/budget-decision-v1' THEN 'core/intervention-decision-v1'
       ELSE schema_id
   END,
       source_id = CASE source_id
       WHEN 'core/budget-review' THEN 'core/intervention'
       ELSE source_id
   END
 WHERE schema_id IN ('core/budget-review-requested-v1', 'core/budget-decision-v1')
    OR source_id = 'core/budget-review';

UPDATE proxima_core.change_event
   SET entity_schema_id = CASE entity_schema_id
       WHEN 'core/budget-review-requested-v1' THEN 'core/intervention-requested-v1'
       WHEN 'core/budget-decision-v1' THEN 'core/intervention-decision-v1'
       ELSE entity_schema_id
   END
 WHERE entity_schema_id IN ('core/budget-review-requested-v1', 'core/budget-decision-v1');

UPDATE proxima_core.cited_objects
   SET schema_id = CASE schema_id
       WHEN 'core/budget-review-requested-object-v1' THEN 'core/intervention-requested-object-v1'
       WHEN 'core/budget-decision-object-v1' THEN 'core/intervention-decision-object-v1'
       ELSE schema_id
   END
 WHERE schema_id IN (
       'core/budget-review-requested-object-v1',
       'core/budget-decision-object-v1'
   );

UPDATE proxima_core.citation_mappings
   SET schema_id = CASE schema_id
       WHEN 'core/budget-review-requested-whole-v1' THEN 'core/intervention-requested-whole-v1'
       WHEN 'core/budget-decision-whole-v1' THEN 'core/intervention-decision-whole-v1'
       ELSE schema_id
   END
 WHERE schema_id IN (
       'core/budget-review-requested-whole-v1',
       'core/budget-decision-whole-v1'
   );

UPDATE proxima_core.source_batches
   SET source_id = 'core/intervention'
 WHERE source_id = 'core/budget-review';

UPDATE proxima_core.edges
   SET relation = 'core/receives-intervention-request'
 WHERE relation = 'core/receives-budget-review';

UPDATE proxima_core.personality_wake_entries
   SET substrate_tool_palette =
           array_replace(substrate_tool_palette,
                         'core/emit_budget_decision',
                         'core/emit_intervention_decision'),
       workspace_tool_palette =
           array_replace(workspace_tool_palette,
                         'core/emit_budget_decision',
                         'core/emit_intervention_decision')
 WHERE 'core/emit_budget_decision' = ANY(substrate_tool_palette)
    OR 'core/emit_budget_decision' = ANY(workspace_tool_palette);
