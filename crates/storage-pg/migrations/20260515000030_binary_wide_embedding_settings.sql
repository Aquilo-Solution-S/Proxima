-- Embedding settings are binary-wide. Inference targets and tier bindings remain
-- Owner-scoped; vector infrastructure does not.

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM (
            SELECT
                vendor,
                model_id,
                COUNT(DISTINCT jsonb_build_object(
                    'base_url', base_url,
                    'caps_dim', caps_dim,
                    'caps_matryoshka', caps_matryoshka,
                    'secret_ref', secret_ref
                )) AS variants
            FROM proxima_core.embedding_models
            GROUP BY vendor, model_id
        ) duplicate_models
        WHERE duplicate_models.variants > 1
    ) THEN
        RAISE EXCEPTION
            'cannot collapse owner-scoped embedding_models: conflicting duplicate vendor/model_id configs';
    END IF;
END
$$;

CREATE TABLE proxima_core.embedding_models_binary (
    vendor text NOT NULL,
    model_id text NOT NULL,
    base_url text NOT NULL,
    caps_dim int NOT NULL,
    caps_matryoshka boolean NOT NULL DEFAULT false,
    secret_ref text,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT embedding_models_binary_pkey PRIMARY KEY (vendor, model_id),
    CONSTRAINT embedding_models_binary_caps_dim_positive_chk CHECK (caps_dim > 0)
);

INSERT INTO proxima_core.embedding_models_binary (
    vendor,
    model_id,
    base_url,
    caps_dim,
    caps_matryoshka,
    secret_ref,
    created_at
)
SELECT
    vendor,
    model_id,
    base_url,
    caps_dim,
    caps_matryoshka,
    secret_ref,
    created_at
FROM (
    SELECT
        em.*,
        row_number() OVER (
            PARTITION BY vendor, model_id
            ORDER BY
                (
                    owner_principal_kind::text = 'User'
                    AND owner_principal_id = '00000000-0000-0000-0000-000000000000'::uuid
                    AND owner_org_id = '00000000-0000-0000-0000-000000000000'::uuid
                ) DESC,
                created_at DESC,
                owner_principal_kind::text,
                owner_principal_id,
                owner_org_id
        ) AS rn
    FROM proxima_core.embedding_models em
) ranked
WHERE rn = 1;

CREATE TABLE proxima_core.embedding_active_binary (
    singleton boolean NOT NULL DEFAULT true,
    vendor text NOT NULL,
    model_id text NOT NULL,
    set_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT embedding_active_binary_pkey PRIMARY KEY (singleton),
    CONSTRAINT embedding_active_binary_singleton_chk CHECK (singleton),
    CONSTRAINT embedding_active_binary_model_fk FOREIGN KEY (vendor, model_id)
        REFERENCES proxima_core.embedding_models_binary (vendor, model_id)
        ON DELETE CASCADE
);

INSERT INTO proxima_core.embedding_active_binary (singleton, vendor, model_id, set_at)
SELECT true, vendor, model_id, set_at
FROM proxima_core.embedding_active
ORDER BY
    (
        owner_principal_kind::text = 'User'
        AND owner_principal_id = '00000000-0000-0000-0000-000000000000'::uuid
        AND owner_org_id = '00000000-0000-0000-0000-000000000000'::uuid
    ) DESC,
    set_at DESC,
    owner_principal_kind::text,
    owner_principal_id,
    owner_org_id
LIMIT 1;

DROP TABLE proxima_core.embedding_active;
DROP TABLE proxima_core.embedding_models;

ALTER TABLE proxima_core.embedding_models_binary RENAME TO embedding_models;
ALTER TABLE proxima_core.embedding_models
    RENAME CONSTRAINT embedding_models_binary_pkey TO embedding_models_pkey;
ALTER TABLE proxima_core.embedding_models
    RENAME CONSTRAINT embedding_models_binary_caps_dim_positive_chk TO embedding_models_caps_dim_positive_chk;

ALTER TABLE proxima_core.embedding_active_binary RENAME TO embedding_active;
ALTER TABLE proxima_core.embedding_active
    RENAME CONSTRAINT embedding_active_binary_pkey TO embedding_active_pkey;
ALTER TABLE proxima_core.embedding_active
    RENAME CONSTRAINT embedding_active_binary_singleton_chk TO embedding_active_singleton_chk;
ALTER TABLE proxima_core.embedding_active
    RENAME CONSTRAINT embedding_active_binary_model_fk TO embedding_active_model_fk;
