-- Phase-1b hard greenfield: replace llm_models + tier_bindings with
-- inference_targets + inference_tier_bindings. No backfill; rows in the
-- old LLM-side tables are discarded. Embedding tables are unchanged.

DROP TABLE IF EXISTS proxima_core.tier_bindings;
DROP TABLE IF EXISTS proxima_core.llm_models;

CREATE TABLE proxima_core.inference_targets (
    owner_principal_kind text NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    target_ref text NOT NULL,
    kind text NOT NULL,
    config jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        target_ref
    ),
    CONSTRAINT inference_targets_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT inference_targets_kind_chk
        CHECK (kind IN ('local_cli', 'remote_model')),
    CONSTRAINT inference_targets_target_ref_nonempty_chk
        CHECK (length(trim(target_ref)) > 0)
);

CREATE TABLE proxima_core.inference_tier_bindings (
    owner_principal_kind text NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    tier text NOT NULL,
    target_ref text NOT NULL,
    bound_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        tier
    ),
    FOREIGN KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        target_ref
    )
        REFERENCES proxima_core.inference_targets
        (owner_principal_kind, owner_principal_id, owner_org_id, target_ref),
    CONSTRAINT inference_tier_bindings_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT inference_tier_bindings_tier_chk
        CHECK (tier IN ('fast', 'standard', 'deep'))
);
