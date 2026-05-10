-- Mapping from (master_token_id, owner) to the per-token shell-author
-- personality. Persists across token revocation: once minted, the
-- identity row stays so authored Facts retain their provenance even if
-- the token UUID is later removed from the auth store. New tokens
-- always mint new rows.

-- The personality table's primary key is composite
-- (owner_principal_kind, owner_principal_id, owner_org_id, personality_instance_id)
-- with no standalone uniqueness on personality_instance_id. The new
-- master_token_personality FK below targets personality_instance_id
-- alone, so add a UNIQUE constraint here. UUIDv7 collisions are
-- effectively zero in any case; this just lets PG accept the FK.
ALTER TABLE proxima_core.personality
    ADD CONSTRAINT personality_instance_id_uq UNIQUE (personality_instance_id);

CREATE TABLE proxima_core.master_token_personality (
    master_token_id          uuid NOT NULL,
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    personality_instance_id  uuid NOT NULL
        REFERENCES proxima_core.personality(personality_instance_id),
    created_at               timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT master_token_personality_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    PRIMARY KEY (master_token_id, owner_principal_kind, owner_principal_id, owner_org_id)
);

CREATE UNIQUE INDEX idx_master_token_personality_instance
    ON proxima_core.master_token_personality (personality_instance_id);
