-- Reference-example storage only. This schema is deliberately separate from
-- Proxima's core schema and must be provisioned by the deployment owner.
CREATE SCHEMA IF NOT EXISTS proxima_durable_intake;

CREATE TABLE IF NOT EXISTS proxima_durable_intake.decisions (
    event_id TEXT NOT NULL CHECK (length(event_id) > 0),
    payload_digest TEXT NOT NULL CHECK (payload_digest ~ '^[0-9a-f]{64}$'),
    raw_payload BYTEA NOT NULL,
    subject TEXT NOT NULL CHECK (length(subject) > 0),
    stream_sequence BIGINT NOT NULL CHECK (stream_sequence >= 0),
    delivered_count BIGINT NOT NULL CHECK (delivered_count > 0),
    decision_at TIMESTAMPTZ NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('accepted', 'rejected')),
    reason TEXT,
    is_primary BOOLEAN NOT NULL,
    original_payload_digest TEXT,
    PRIMARY KEY (event_id, payload_digest),
    CONSTRAINT decisions_outcome_reason CHECK (
        (outcome = 'accepted' AND reason IS NULL)
        OR (outcome = 'rejected' AND reason IS NOT NULL AND length(trim(reason)) > 0)
    ),
    CONSTRAINT decisions_primary_shape CHECK (
        (is_primary AND original_payload_digest IS NULL)
        OR (
            NOT is_primary
            AND outcome = 'rejected'
            AND original_payload_digest IS NOT NULL
            AND original_payload_digest <> payload_digest
        )
    ),
    CONSTRAINT decisions_original_digest_format CHECK (
        original_payload_digest IS NULL
        OR original_payload_digest ~ '^[0-9a-f]{64}$'
    ),
    CONSTRAINT decisions_conflict_original_fk
        FOREIGN KEY (event_id, original_payload_digest)
        REFERENCES proxima_durable_intake.decisions (event_id, payload_digest)
);

CREATE UNIQUE INDEX IF NOT EXISTS decisions_one_primary
    ON proxima_durable_intake.decisions (event_id)
    WHERE is_primary;
