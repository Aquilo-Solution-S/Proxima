CREATE TABLE proxima_core.owner_fact_retention (
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    retention_seconds BIGINT NOT NULL CHECK (retention_seconds > 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id)
);
