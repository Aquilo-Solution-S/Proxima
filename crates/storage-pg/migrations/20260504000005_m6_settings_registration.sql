-- M6 — settings registration tables.
-- See docs/10 §"Model registration" for the runtime/build-time split.
--
-- Replaces the m6.17 TOML-based AppConfig persistence with DB-backed
-- per-Owner storage. AppConfig types in proxima-shell/src-tauri stay
-- as the in-memory/wire DTO; PgConfigStore (m6.21) maps columns
-- here ↔ those types.
--
-- Caps are flat columns (no JSONB) — co-evolve with Rust LlmCaps /
-- EmbedCaps definitions. New caps land via ALTER TABLE ADD COLUMN.

----------------------------------------------------------
-- llm_models — registered LLM model entries (10 §Model
-- registration). Per-Owner; v1 desktop populates with
-- Uuid::nil() sentinels via build_engine, v1.1+ uses real
-- org_ids without schema change.
----------------------------------------------------------
CREATE TABLE proxima_core.llm_models (
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    vendor                   text NOT NULL,
    model_id                 text NOT NULL,
    dialect                  text NOT NULL,
    base_url                 text NOT NULL,
    caps_tool_use            boolean NOT NULL DEFAULT false,
    caps_json_mode           boolean NOT NULL DEFAULT false,
    caps_long_context        boolean NOT NULL DEFAULT false,
    caps_vision              boolean NOT NULL DEFAULT false,
    secret_ref               text,
    created_at               timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, vendor, model_id),
    CONSTRAINT llm_models_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT llm_models_dialect_chk
        CHECK (dialect IN ('anthropic', 'openai'))
);

----------------------------------------------------------
-- embedding_models — registered embedding model entries.
-- caps_dim must match the storage embeddings.dim column for any
-- already-written vectors; mismatch is a runtime concern
-- (validate_config + engine.embed_requires_union later, m6.22+).
----------------------------------------------------------
CREATE TABLE proxima_core.embedding_models (
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    vendor                   text NOT NULL,
    model_id                 text NOT NULL,
    base_url                 text NOT NULL,
    caps_dim                 int NOT NULL,
    caps_matryoshka          boolean NOT NULL DEFAULT false,
    secret_ref               text,
    created_at               timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, vendor, model_id),
    CONSTRAINT embedding_models_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT embedding_models_caps_dim_positive_chk
        CHECK (caps_dim > 0)
);

----------------------------------------------------------
-- tier_bindings — tier → (vendor, model_id) per Owner. PK on
-- (owner..., tier) enforces at most one binding per tier.
-- Composite FK to llm_models keeps tier-bound model existence
-- consistent.
--
-- Caps satisfaction (engine.tier_requires_union) is validated in
-- Rust at boot + on bind. The DB only enforces existence.
----------------------------------------------------------
CREATE TABLE proxima_core.tier_bindings (
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    tier                     text NOT NULL,
    vendor                   text NOT NULL,
    model_id                 text NOT NULL,
    bound_at                 timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, tier),
    FOREIGN KEY (owner_principal_kind, owner_principal_id, owner_org_id, vendor, model_id)
        REFERENCES proxima_core.llm_models
        (owner_principal_kind, owner_principal_id, owner_org_id, vendor, model_id),
    CONSTRAINT tier_bindings_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT tier_bindings_tier_chk
        CHECK (tier IN ('fast', 'standard', 'deep'))
);

----------------------------------------------------------
-- embedding_active — single active embedding model per Owner.
-- PK on the owner triple = at most one row = enforces the
-- single-global-active rule (10 §"Composite embedding selection").
-- Per-flavor embedding requirements (m6.22+) validate against this.
----------------------------------------------------------
CREATE TABLE proxima_core.embedding_active (
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    vendor                   text NOT NULL,
    model_id                 text NOT NULL,
    set_at                   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id),
    FOREIGN KEY (owner_principal_kind, owner_principal_id, owner_org_id, vendor, model_id)
        REFERENCES proxima_core.embedding_models
        (owner_principal_kind, owner_principal_id, owner_org_id, vendor, model_id),
    CONSTRAINT embedding_active_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group'))
);
