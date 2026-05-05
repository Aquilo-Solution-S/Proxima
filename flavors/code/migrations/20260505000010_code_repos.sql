-- M6.S2 — code flavor repo registry.
-- Tracks local git repos the user has registered for ingestion.
-- One row per (owner, canonical_path); cursor advances on each successful poll.

CREATE TABLE proxima_code.repos (
    owner_principal_kind     text NOT NULL,
    owner_principal_id       uuid NOT NULL,
    owner_org_id             uuid NOT NULL,
    repo_id                  uuid NOT NULL,
    canonical_path           text NOT NULL,
    display_name             text NOT NULL,
    last_cursor              bytea,
    last_polled_at           timestamptz,
    created_at               timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, repo_id),
    CONSTRAINT repos_principal_kind_chk
        CHECK (owner_principal_kind IN ('User', 'Group')),
    CONSTRAINT repos_unique_path
        UNIQUE (owner_principal_kind, owner_principal_id, owner_org_id, canonical_path)
);
