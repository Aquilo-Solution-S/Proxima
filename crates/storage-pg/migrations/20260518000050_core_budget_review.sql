CREATE TYPE proxima_core.budget_decision_kind AS ENUM (
    'continue',
    'stop',
    'redirect',
    'decompose',
    'accept_terminal'
);

ALTER TABLE proxima_core.personality_wake_entries
    ADD COLUMN budgeter_personality_instance_id uuid,
    ADD COLUMN budget_extension_rounds integer DEFAULT 0 NOT NULL,
    ADD COLUMN budget_hard_cap_rounds integer DEFAULT 0 NOT NULL,
    ADD COLUMN budget_progress_contract text DEFAULT ''::text NOT NULL,
    ADD CONSTRAINT personality_wake_entries_budget_rounds_chk
        CHECK (budget_extension_rounds >= 0 AND budget_hard_cap_rounds >= 0),
    ADD CONSTRAINT personality_wake_entries_budget_policy_chk
        CHECK (
            (budgeter_personality_instance_id IS NULL
             AND budget_extension_rounds = 0
             AND budget_hard_cap_rounds = 0
             AND budget_progress_contract = '')
            OR
            (budgeter_personality_instance_id IS NOT NULL
             AND budget_extension_rounds > 0
             AND budget_hard_cap_rounds >= budget_extension_rounds
             AND length(budget_progress_contract) > 0)
        );

CREATE TABLE proxima_core.budget_review_requested_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    original_invocation_id uuid NOT NULL,
    original_wake_entry_id uuid NOT NULL,
    original_personality_instance_id uuid NOT NULL,
    original_change_event_seq uuid NOT NULL,
    triggering_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    wake_trace_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    target_budgeter_personality_instance_id uuid NOT NULL,
    max_rounds integer NOT NULL,
    rounds_used integer NOT NULL,
    budget_extension_rounds integer NOT NULL,
    budget_hard_cap_rounds integer NOT NULL,
    continued_rounds_used integer DEFAULT 0 NOT NULL,
    active_goal_ids uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    progress_contract text NOT NULL,
    requested_at timestamp with time zone DEFAULT now() NOT NULL,
    idempotency_key text NOT NULL,
    CONSTRAINT budget_review_rounds_chk
        CHECK (
            max_rounds >= 0
            AND rounds_used >= 0
            AND budget_extension_rounds > 0
            AND budget_hard_cap_rounds >= budget_extension_rounds
            AND continued_rounds_used >= 0
        ),
    CONSTRAINT budget_review_progress_contract_chk CHECK (length(progress_contract) > 0),
    CONSTRAINT budget_review_idempotency_key_chk CHECK (length(idempotency_key) > 0)
);

CREATE UNIQUE INDEX budget_review_requested_invocation_uq
    ON proxima_core.budget_review_requested_v1 (original_invocation_id);

CREATE INDEX budget_review_requested_target_idx
    ON proxima_core.budget_review_requested_v1 (target_budgeter_personality_instance_id);

CREATE TABLE proxima_core.budget_decision_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    budget_request_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    decision proxima_core.budget_decision_kind NOT NULL,
    grant_rounds integer,
    redirect_personality_instance_id uuid,
    rationale text NOT NULL,
    decided_at timestamp with time zone DEFAULT now() NOT NULL,
    idempotency_key text NOT NULL,
    CONSTRAINT budget_decision_rounds_chk CHECK (grant_rounds IS NULL OR grant_rounds >= 0),
    CONSTRAINT budget_decision_rationale_chk CHECK (length(rationale) > 0),
    CONSTRAINT budget_decision_idempotency_key_chk CHECK (length(idempotency_key) > 0)
);

CREATE UNIQUE INDEX budget_decision_idempotency_uq
    ON proxima_core.budget_decision_v1 (budget_request_memory_id, idempotency_key);

CREATE INDEX budget_decision_request_idx
    ON proxima_core.budget_decision_v1 (budget_request_memory_id);
