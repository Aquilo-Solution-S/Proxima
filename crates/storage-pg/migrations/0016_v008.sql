-- Proxima core schema — v0.0.8 durable delegated command authority.
--
-- Queue rows persist only delegation_id. The grant carries the authenticated
-- subject, exact Owner, one registered tool/action, a non-managing role
-- ceiling, bearer expiry, and auth epoch. delegation_id is the secret,
-- redeemable queue authority; no source host bearer or serialized AuthzContext
-- is stored.

CREATE TYPE proxima_core.access_ceiling AS ENUM (
    'none',
    'fact',
    'abstraction',
    'perspective',
    'goal'
);

CREATE TABLE proxima_core.delegated_authority_grants (
    delegation_id uuid PRIMARY KEY,
    subject_user_id uuid NOT NULL,
    owner_kind proxima_core.owner_ref_kind NOT NULL,
    owner_id uuid,
    tool_name text NOT NULL,
    action_name text,
    read_ceiling proxima_core.access_ceiling NOT NULL,
    write_ceiling proxima_core.access_ceiling NOT NULL,
    expires_at timestamptz NOT NULL,
    auth_epoch bigint NOT NULL,
    issued_at timestamptz NOT NULL,
    revoked_at timestamptz,
    revoked_by_user_id uuid,
    CONSTRAINT delegated_authority_owner_ref_shape_chk CHECK (
        (owner_kind = 'world' AND owner_id IS NULL)
        OR (owner_kind IN ('personal', 'group') AND owner_id IS NOT NULL)
    ),
    CONSTRAINT delegated_authority_delegation_id_not_nil_chk CHECK (
        delegation_id <> '00000000-0000-0000-0000-000000000000'::uuid
    ),
    CONSTRAINT delegated_authority_subject_not_nil_chk CHECK (
        subject_user_id <> '00000000-0000-0000-0000-000000000000'::uuid
    ),
    CONSTRAINT delegated_authority_owner_id_not_nil_chk CHECK (
        owner_id IS NULL
        OR owner_id <> '00000000-0000-0000-0000-000000000000'::uuid
    ),
    CONSTRAINT delegated_authority_tool_name_chk CHECK (
        tool_name = btrim(tool_name)
        AND tool_name <> ''
        AND tool_name ~ '^[A-Za-z0-9_.-]+$'
        AND strpos(tool_name, '..') = 0
    ),
    CONSTRAINT delegated_authority_action_name_chk CHECK (
        action_name IS NULL
        OR (
            action_name = btrim(action_name)
            AND action_name <> ''
            AND action_name ~ '^[A-Za-z0-9_.-]+$'
            AND strpos(action_name, '..') = 0
        )
    ),
    CONSTRAINT delegated_authority_command_length_chk CHECK (
        char_length(tool_name)
        + CASE WHEN action_name IS NULL THEN 0 ELSE 1 + char_length(action_name) END
        <= 200
    ),
    CONSTRAINT delegated_authority_role_ceiling_chk CHECK (
        (CASE write_ceiling
            WHEN 'none' THEN 0
            WHEN 'fact' THEN 1
            WHEN 'abstraction' THEN 2
            WHEN 'perspective' THEN 3
            WHEN 'goal' THEN 4
         END)
        <=
        (CASE read_ceiling
            WHEN 'none' THEN 0
            WHEN 'fact' THEN 1
            WHEN 'abstraction' THEN 2
            WHEN 'perspective' THEN 3
            WHEN 'goal' THEN 4
         END)
    ),
    CONSTRAINT delegated_authority_auth_epoch_chk CHECK (auth_epoch >= 0),
    CONSTRAINT delegated_authority_expiry_chk CHECK (expires_at > issued_at),
    CONSTRAINT delegated_authority_revocation_shape_chk CHECK (
        (revoked_at IS NULL AND revoked_by_user_id IS NULL)
        OR (
            revoked_at IS NOT NULL
            AND revoked_by_user_id IS NOT NULL
            AND revoked_at >= issued_at
            AND revoked_by_user_id <> '00000000-0000-0000-0000-000000000000'::uuid
        )
    )
);

CREATE INDEX delegated_authority_owner_idx
    ON proxima_core.delegated_authority_grants
        (owner_kind, owner_id, issued_at, delegation_id);

-- Not partial: personal-owner erasure must find revoked and expired grants
-- issued by the deleted subject even when those grants target another owner.
CREATE INDEX delegated_authority_subject_idx
    ON proxima_core.delegated_authority_grants
        (subject_user_id, issued_at, delegation_id);

-- The durable grant is immutable except for its single null -> revoked
-- transition. A privileged raw UPDATE cannot silently widen owner, command,
-- role, epoch, or expiry after the validated service issued the row.
CREATE FUNCTION proxima_core.delegated_authority_grants_revoke_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW IS NOT DISTINCT FROM OLD THEN
        RETURN NEW;
    END IF;

    IF OLD.revoked_at IS NOT NULL
       OR OLD.revoked_by_user_id IS NOT NULL
       OR NEW.revoked_at IS NULL
       OR NEW.revoked_by_user_id IS NULL
       OR ROW(
            NEW.delegation_id,
            NEW.subject_user_id,
            NEW.owner_kind,
            NEW.owner_id,
            NEW.tool_name,
            NEW.action_name,
            NEW.read_ceiling,
            NEW.write_ceiling,
            NEW.expires_at,
            NEW.auth_epoch,
            NEW.issued_at
          ) IS DISTINCT FROM ROW(
            OLD.delegation_id,
            OLD.subject_user_id,
            OLD.owner_kind,
            OLD.owner_id,
            OLD.tool_name,
            OLD.action_name,
            OLD.read_ceiling,
            OLD.write_ceiling,
            OLD.expires_at,
            OLD.auth_epoch,
            OLD.issued_at
          ) THEN
        RAISE EXCEPTION
            'delegated authority grant is immutable except for first revocation';
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER delegated_authority_grants_revoke_only
    BEFORE UPDATE ON proxima_core.delegated_authority_grants
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.delegated_authority_grants_revoke_only();

ALTER TABLE proxima_core.compliance_audit_log
    ADD COLUMN delegated_authority_grants_count bigint NOT NULL DEFAULT 0;

ALTER TABLE proxima_core.compliance_audit_log
    DROP CONSTRAINT compliance_audit_log_no_negative_counts_chk;

ALTER TABLE proxima_core.compliance_audit_log
    ADD CONSTRAINT compliance_audit_log_no_negative_counts_chk CHECK (
        memories_count >= 0 AND goals_count >= 0 AND edges_count >= 0
        AND fact_entities_count >= 0 AND receipts_count >= 0
        AND source_batches_count >= 0 AND citations_count >= 0
        AND cited_objects_count >= 0 AND source_cursors_count >= 0
        AND embeddings_count >= 0 AND embedding_jobs_count >= 0
        AND mcp_call_rows_count >= 0 AND change_events_count >= 0
        AND redacted_edge_targets_count >= 0 AND suppressed_keys_count >= 0
        AND delegated_authority_grants_count >= 0
    );

COMMENT ON TABLE proxima_core.delegated_authority_grants IS
'Bearer-bounded, exact-owner, one-command grants for durable workers. delegation_id is secret redeemable queue authority; no source host bearer or serialized AuthzContext is stored. Redeem re-resolves membership and auth epoch. Expired and revoked rows remain audit evidence until owner erasure; source-scope erasure never deletes them.';
