CREATE TABLE proxima_core.a2p_invocations (
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    operator_id              text NOT NULL,
    prompt_version           text NOT NULL,
    model_id                 text NOT NULL,
    personality_id           text NOT NULL,
    personality_state_hash   bytea NOT NULL,
    context_hash             bytea NOT NULL,
    input_hash               bytea NOT NULL,
    head_memory_id           uuid REFERENCES proxima_core.memories(memory_id),
    run_at                   timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT a2p_invocations_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT a2p_invocations_personality_hash_chk
        CHECK (octet_length(personality_state_hash) = 32),
    CONSTRAINT a2p_invocations_context_hash_chk
        CHECK (octet_length(context_hash) = 32),
    CONSTRAINT a2p_invocations_input_hash_chk
        CHECK (octet_length(input_hash) = 32),
    -- Owner scope is principal-only — must match `load_a2p_abstractions`,
    -- which reads abstractions by principal regardless of `org_id`. Including
    -- `org_id` here would make the same-principal/different-org case miss
    -- prior invocations and produce duplicate Perspectives without lineage.
    PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        operator_id,
        prompt_version,
        model_id,
        personality_id,
        personality_state_hash,
        context_hash,
        input_hash
    )
);

CREATE INDEX idx_a2p_invocations_owner_run
    ON proxima_core.a2p_invocations
       (owner_principal_kind, owner_principal_id, run_at DESC);
