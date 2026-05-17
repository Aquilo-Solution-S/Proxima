CREATE TYPE proxima_core.approval_target_kind AS ENUM (
    'fact',
    'abstraction',
    'perspective',
    'goal'
);

CREATE TYPE proxima_core.approval_voter_kind AS ENUM (
    'personality',
    'shell_author'
);

CREATE TYPE proxima_core.approval_vote_verdict AS ENUM (
    'approved',
    'request_changes',
    'abstain'
);

CREATE TYPE proxima_core.approval_decision AS ENUM (
    'approved',
    'blocked'
);

CREATE TYPE proxima_core.approval_requirement_kind AS ENUM (
    'all_of_voters',
    'role_quorum'
);

CREATE TABLE proxima_core.approval_policy_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    target_kind proxima_core.approval_target_kind NOT NULL,
    target_memory_id uuid REFERENCES proxima_core.memories(memory_id),
    target_goal_id uuid REFERENCES proxima_core.goals(goal_id),
    title text NOT NULL,
    summary text NOT NULL,
    eligible_voters_json jsonb NOT NULL,
    requirements_json jsonb NOT NULL,
    idempotency_key text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT approval_policy_v1_target_chk CHECK (
        ((target_kind = 'goal') AND target_memory_id IS NULL AND target_goal_id IS NOT NULL)
        OR ((target_kind <> 'goal') AND target_memory_id IS NOT NULL AND target_goal_id IS NULL)
    ),
    CONSTRAINT approval_policy_v1_title_chk CHECK (char_length(title) BETWEEN 1 AND 300),
    CONSTRAINT approval_policy_v1_summary_chk CHECK (char_length(summary) BETWEEN 1 AND 4000),
    CONSTRAINT approval_policy_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240)
);

CREATE TABLE proxima_core.approval_vote_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    policy_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    voter_key text NOT NULL,
    voter_kind proxima_core.approval_voter_kind NOT NULL,
    role text,
    personality_instance_id uuid,
    self_perspective_memory_id uuid REFERENCES proxima_core.memories(memory_id),
    master_token_id uuid,
    verdict proxima_core.approval_vote_verdict NOT NULL,
    rationale text NOT NULL,
    idempotency_key text NOT NULL,
    voted_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT approval_vote_v1_voter_key_chk CHECK (char_length(voter_key) BETWEEN 1 AND 120),
    CONSTRAINT approval_vote_v1_role_chk CHECK (role IS NULL OR char_length(role) BETWEEN 1 AND 120),
    CONSTRAINT approval_vote_v1_rationale_chk CHECK (char_length(rationale) BETWEEN 1 AND 4000),
    CONSTRAINT approval_vote_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240),
    CONSTRAINT approval_vote_v1_voter_shape_chk CHECK (
        ((voter_kind = 'personality') AND personality_instance_id IS NOT NULL
            AND self_perspective_memory_id IS NOT NULL AND master_token_id IS NULL)
        OR ((voter_kind = 'shell_author') AND personality_instance_id IS NULL
            AND self_perspective_memory_id IS NOT NULL AND master_token_id IS NOT NULL)
    )
);

CREATE TABLE proxima_core.approval_decision_v1 (
    memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
    policy_memory_id uuid NOT NULL REFERENCES proxima_core.memories(memory_id),
    target_kind proxima_core.approval_target_kind NOT NULL,
    target_memory_id uuid REFERENCES proxima_core.memories(memory_id),
    target_goal_id uuid REFERENCES proxima_core.goals(goal_id),
    decision proxima_core.approval_decision NOT NULL,
    reason text NOT NULL,
    counted_votes_json jsonb NOT NULL,
    idempotency_key text NOT NULL,
    decided_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT approval_decision_v1_target_chk CHECK (
        ((target_kind = 'goal') AND target_memory_id IS NULL AND target_goal_id IS NOT NULL)
        OR ((target_kind <> 'goal') AND target_memory_id IS NOT NULL AND target_goal_id IS NULL)
    ),
    CONSTRAINT approval_decision_v1_reason_chk CHECK (char_length(reason) BETWEEN 1 AND 4000),
    CONSTRAINT approval_decision_v1_idempotency_key_chk CHECK (char_length(idempotency_key) BETWEEN 1 AND 240)
);

CREATE INDEX idx_approval_policy_v1_target_memory
    ON proxima_core.approval_policy_v1 (target_memory_id);

CREATE INDEX idx_approval_policy_v1_target_goal
    ON proxima_core.approval_policy_v1 (target_goal_id);

CREATE INDEX idx_approval_vote_v1_policy_latest
    ON proxima_core.approval_vote_v1 (policy_memory_id, voter_key, voted_at DESC, memory_id DESC);

CREATE INDEX idx_approval_decision_v1_policy
    ON proxima_core.approval_decision_v1 (policy_memory_id, decided_at DESC);
