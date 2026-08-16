-- Membership, lexical language set, and remaining core sidecars.

CREATE TYPE proxima_core.membership_relation AS ENUM (
    'admin',
    'editor',
    'viewer',
    'ingest'
);

CREATE TABLE proxima_core.group_memberships (
    group_id uuid NOT NULL,
    member_user_id uuid NOT NULL,
    relation proxima_core.membership_relation NOT NULL,
    PRIMARY KEY (group_id, member_user_id, relation)
);

CREATE TABLE proxima_core.lexical_languages (
    config regconfig PRIMARY KEY
);

INSERT INTO proxima_core.lexical_languages (config)
VALUES ('english'::regconfig)
ON CONFLICT DO NOTHING;

CREATE TYPE proxima_core.task_priority AS ENUM (
    'Low',
    'Medium',
    'High'
);

CREATE TYPE proxima_core.interpretation_subject_kind AS ENUM (
    'Fact',
    'Abstraction',
    'Perspective'
);

CREATE TABLE proxima_core.agent_note_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    note_id uuid NOT NULL,
    title text NOT NULL,
    body text NOT NULL,
    tags text[] NOT NULL DEFAULT '{}',
    idempotency_key text
);

CREATE TABLE proxima_core.utterance_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    speaker text NOT NULL,
    conversation_id text NOT NULL,
    text text NOT NULL
);

CREATE TABLE proxima_core.agent_derivation_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    title text NOT NULL,
    body text NOT NULL,
    tags text[] NOT NULL DEFAULT '{}',
    idempotency_key text,
    source_memory_ids uuid[] NOT NULL DEFAULT '{}',
    model_id text NOT NULL,
    client_name text NOT NULL,
    client_version text NOT NULL
);

CREATE TABLE proxima_core.interpretation_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    claim text NOT NULL,
    confidence smallint NOT NULL,
    subject_memory_ids uuid[] NOT NULL DEFAULT '{}',
    subject_kinds proxima_core.interpretation_subject_kind[] NOT NULL DEFAULT '{}',
    model_id text NOT NULL,
    client_name text NOT NULL,
    client_version text NOT NULL
);

CREATE TABLE proxima_core.task_goal_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.goal (t),
    due_at timestamptz,
    priority proxima_core.task_priority
);

-- Upload staging for blob-s3. Not a citation map; citation is memory.blob_id.
CREATE TYPE proxima_core.blob_upload_status AS ENUM (
    'pending',
    'completed',
    'aborted',
    'expired'
);

CREATE TABLE proxima_core.blob_uploads (
    upload_id uuid PRIMARY KEY DEFAULT uuidv7(),
    owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    bucket text NOT NULL,
    object_key text NOT NULL,
    filename text NOT NULL,
    mime text NOT NULL,
    expected_byte_len bigint NOT NULL,
    status proxima_core.blob_upload_status NOT NULL DEFAULT 'pending',
    blob_id uuid REFERENCES proxima_core.blob (blob_id),
    sha256 bytea,
    etag text,
    error_message text,
    expires_at timestamptz NOT NULL,
    completed_at timestamptz,
    aborted_at timestamptz
);

CREATE INDEX blob_uploads_owner_status_idx
    ON proxima_core.blob_uploads (owner_id, status);


