-- S3-backed generic uploaded-blob cited objects.
-- See docs/11-citations.md §"Large artefact storage".

CREATE TYPE proxima_core.cited_object_upload_status AS ENUM (
    'pending',
    'completed',
    'aborted',
    'expired'
);

CREATE TABLE proxima_core.cited_uploaded_blob_v1 (
    cited_object_id             uuid PRIMARY KEY REFERENCES proxima_core.cited_objects(cited_object_id),
    bucket                      text NOT NULL,
    object_key                  text NOT NULL,
    sha256                      bytea NOT NULL,
    byte_len                    bigint NOT NULL,
    mime                        text NOT NULL,
    filename                    text NOT NULL,
    etag                        text,
    uploaded_at                 timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT cited_uploaded_blob_sha256_len_chk CHECK (octet_length(sha256) = 32),
    CONSTRAINT cited_uploaded_blob_byte_len_chk CHECK (byte_len >= 0),
    CONSTRAINT cited_uploaded_blob_object_unique UNIQUE (bucket, object_key)
);

CREATE TABLE proxima_core.cited_object_uploads (
    owner_principal_kind        proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id          uuid NOT NULL,
    owner_org_id                uuid NOT NULL,
    upload_id                   uuid NOT NULL,
    bucket                      text NOT NULL,
    object_key                  text NOT NULL,
    filename                    text NOT NULL,
    mime                        text NOT NULL,
    expected_byte_len           bigint NOT NULL,
    status                      proxima_core.cited_object_upload_status NOT NULL DEFAULT 'pending',
    cited_object_id             uuid REFERENCES proxima_core.cited_objects(cited_object_id),
    prepared_at                 timestamptz NOT NULL DEFAULT now(),
    expires_at                  timestamptz NOT NULL,
    completed_at                timestamptz,
    aborted_at                  timestamptz,
    error_message               text,
    PRIMARY KEY (owner_principal_kind, owner_principal_id, owner_org_id, upload_id),
    CONSTRAINT cited_object_uploads_expected_len_chk CHECK (expected_byte_len >= 0),
    CONSTRAINT cited_object_uploads_terminal_shape_chk CHECK (
        (status = 'completed' AND cited_object_id IS NOT NULL AND completed_at IS NOT NULL)
        OR (status <> 'completed' AND completed_at IS NULL)
    )
);

CREATE INDEX cited_object_uploads_upload_id_idx
    ON proxima_core.cited_object_uploads (upload_id);

CREATE INDEX cited_object_uploads_pending_expiry_idx
    ON proxima_core.cited_object_uploads (expires_at)
    WHERE status = 'pending';
