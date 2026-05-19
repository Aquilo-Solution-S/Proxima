CREATE TABLE proxima_core.read_scope_matrix (
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    reader_personality_instance_id uuid NOT NULL,
    readable_personality_instance_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT read_scope_matrix_no_identity_chk CHECK (reader_personality_instance_id <> readable_personality_instance_id),
    PRIMARY KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        reader_personality_instance_id,
        readable_personality_instance_id
    ),
    FOREIGN KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        reader_personality_instance_id
    ) REFERENCES proxima_core.personality (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id
    ),
    FOREIGN KEY (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        readable_personality_instance_id
    ) REFERENCES proxima_core.personality (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        personality_instance_id
    )
);

CREATE INDEX idx_read_scope_matrix_readable
    ON proxima_core.read_scope_matrix (
        owner_principal_kind,
        owner_principal_id,
        owner_org_id,
        readable_personality_instance_id
    );
