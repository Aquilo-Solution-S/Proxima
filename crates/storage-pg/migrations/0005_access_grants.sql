-- 0005_access_grants.sql
--
-- Entry-level access model (Phase 1): one persisted, Zanzibar-shaped grant
-- relation `(resource, relation, subject)` plus a denormalized `memories.visibility`
-- fast-path. Collapses the old two-axis RBAC (RoleSet + per-token MemorySpaceGrants)
-- into a single persisted, cross-principal access primitive.
--
-- Append-only: never edit a shipped migration (sqlx checksum). See
-- docs/superpowers/specs/2026-06-27-entry-access-model-design.md.

-- ---------------------------------------------------------------------------
-- Enums
-- ---------------------------------------------------------------------------

CREATE TYPE proxima_core.grant_resource_kind AS ENUM ('space', 'memory');

-- The partial relation lattice vocabulary. owner > editor > viewer; admin,
-- ingest, member are each incomparable.
CREATE TYPE proxima_core.grant_relation AS ENUM
    ('owner', 'admin', 'editor', 'viewer', 'ingest', 'member');

-- public is NOT a grant subject in Phase 1 (public = the visibility flag below),
-- so the subject is always a principal or a group naming a principal.
CREATE TYPE proxima_core.grant_subject_kind AS ENUM ('principal', 'group');

CREATE TYPE proxima_core.grant_state AS ENUM ('active', 'revoked');

CREATE TYPE proxima_core.memory_visibility AS ENUM ('private', 'shared', 'public');

-- ---------------------------------------------------------------------------
-- memories.visibility (denormalized fast-path)
-- ---------------------------------------------------------------------------
--   private  - only space members may access.
--   shared   - cache of ">=1 active entry-level grant exists" (NOT an allow source).
--   public   - world-readable marketplace entry (this one IS a read source-of-truth).
ALTER TABLE proxima_core.memories
    ADD COLUMN visibility proxima_core.memory_visibility NOT NULL DEFAULT 'private';

-- ---------------------------------------------------------------------------
-- access_grants
-- ---------------------------------------------------------------------------

CREATE TABLE proxima_core.access_grants (
    grant_id               uuid PRIMARY KEY,
    -- the owner-space the resource lives in (host-resolved):
    owner_principal_kind   proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id     uuid NOT NULL,
    -- the resource the grant is about (typed so existence + owner can be enforced):
    resource_kind          proxima_core.grant_resource_kind NOT NULL,
    resource_id            uuid,            -- memory_id; NULL when resource_kind='space'
    relation               proxima_core.grant_relation NOT NULL,
    -- the subject the grant is for:
    subject_kind           proxima_core.grant_subject_kind NOT NULL,
    subject_principal_kind proxima_core.owner_principal_kind NOT NULL,
    subject_principal_id   uuid NOT NULL,
    grant_state            proxima_core.grant_state NOT NULL DEFAULT 'active',
    granted_by_personality_instance_id uuid NOT NULL,  -- audit: who granted
    created_at             timestamptz NOT NULL DEFAULT now(),
    revoked_at             timestamptz,

    -- space rows carry no resource_id; memory rows must:
    CONSTRAINT access_grants_resource_chk
        CHECK ((resource_kind = 'space') = (resource_id IS NULL)),
    -- a group subject names a Group principal:
    CONSTRAINT access_grants_group_subject_chk
        CHECK (subject_kind <> 'group' OR subject_principal_kind = 'Group'),
    -- owner is space-only and written ONLY by the init/add/remove-owner ops,
    -- never by the ordinary grant verbs (which reject relation='owner'):
    CONSTRAINT access_grants_owner_space_only_chk
        CHECK (relation <> 'owner' OR resource_kind = 'space'),
    -- member is a space-only relation (group membership):
    CONSTRAINT access_grants_member_space_only_chk
        CHECK (relation <> 'member' OR resource_kind = 'space'),
    -- you join a Group space (never a personal User space):
    CONSTRAINT access_grants_member_group_space_chk
        CHECK (relation <> 'member' OR owner_principal_kind = 'Group'),
    CONSTRAINT access_grants_member_person_subject_chk
        CHECK (relation <> 'member' OR subject_kind = 'principal'),
    -- v1 flat: a member is a PERSON (User), never a Group - no nesting:
    CONSTRAINT access_grants_member_no_nesting_chk
        CHECK (relation <> 'member' OR subject_principal_kind = 'User'),
    CONSTRAINT access_grants_revoked_chk
        CHECK ((grant_state = 'revoked') = (revoked_at IS NOT NULL))
);

CREATE INDEX idx_access_grants_resource
    ON proxima_core.access_grants (resource_kind, resource_id, relation, grant_state);
CREATE INDEX idx_access_grants_subject
    ON proxima_core.access_grants (subject_kind, subject_principal_kind,
                                   subject_principal_id, grant_state);
-- space-grant lookups: resource_id is NULL for space rows, so the resource index
-- is weak there - index by owner instead.
CREATE INDEX idx_access_grants_space
    ON proxima_core.access_grants (owner_principal_kind, owner_principal_id,
                                   relation, grant_state)
    WHERE resource_kind = 'space';

-- At most one ACTIVE grant per (memory resource, relation, subject).
CREATE UNIQUE INDEX uq_access_grants_active_memory
    ON proxima_core.access_grants
       (resource_id, relation, subject_kind,
        subject_principal_kind, subject_principal_id)
    WHERE grant_state = 'active' AND resource_kind = 'memory';
-- At most one ACTIVE grant per (space owner, relation, subject). Separate index
-- because resource_id is NULL for space rows (NULLs compare distinct in UNIQUE).
CREATE UNIQUE INDEX uq_access_grants_active_space
    ON proxima_core.access_grants
       (owner_principal_kind, owner_principal_id, relation, subject_kind,
        subject_principal_kind, subject_principal_id)
    WHERE grant_state = 'active' AND resource_kind = 'space';

-- ---------------------------------------------------------------------------
-- Existence + owner-match + live-target trigger
-- ---------------------------------------------------------------------------
-- Postgres cannot FK a polymorphic resource_id, so enforce by trigger: a memory
-- grant's target must exist, be LIVE, and have the same owner as the grant.
-- The lock must be FOR SHARE (not FOR KEY SHARE): tombstone is a non-key
-- UPDATE memories SET tombstoned_at taking FOR NO KEY UPDATE, and only
-- FOR SHARE/FOR UPDATE conflict with it - so FOR SHARE serializes a grant-insert
-- against a concurrent tombstone.
CREATE OR REPLACE FUNCTION proxima_core.validate_access_grant()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    target_kind proxima_core.owner_principal_kind;
    target_id   uuid;
BEGIN
    IF NEW.resource_kind = 'memory' THEN
        SELECT m.owner_principal_kind, m.owner_principal_id
          INTO target_kind, target_id
          FROM proxima_core.memories m
         WHERE m.memory_id = NEW.resource_id
           AND m.tombstoned_at IS NULL
         FOR SHARE;
        IF NOT FOUND THEN
            RAISE EXCEPTION
                'access_grant target memory % is absent or tombstoned', NEW.resource_id
                USING ERRCODE = 'foreign_key_violation';
        END IF;
        IF target_kind <> NEW.owner_principal_kind
           OR target_id <> NEW.owner_principal_id THEN
            RAISE EXCEPTION
                'access_grant owner mismatch for memory %', NEW.resource_id
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER trg_validate_access_grant
    BEFORE INSERT OR UPDATE OF resource_kind, resource_id,
        owner_principal_kind, owner_principal_id
    ON proxima_core.access_grants
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.validate_access_grant();

-- ---------------------------------------------------------------------------
-- Tombstone -> revoke grants + reset visibility (same transaction)
-- ---------------------------------------------------------------------------
-- Tombstoning/erasing a memory revokes its active grants AND resets
-- visibility='private' in the same txn. The visibility reset is essential
-- because public access is grantless: revoking grants alone would leave a
-- tombstoned public memory still flagged public and served by marketplace/browse.
-- BEFORE UPDATE so the function may mutate NEW.visibility.
CREATE OR REPLACE FUNCTION proxima_core.revoke_grants_on_tombstone()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.tombstoned_at IS NOT NULL AND OLD.tombstoned_at IS NULL THEN
        UPDATE proxima_core.access_grants
           SET grant_state = 'revoked', revoked_at = now()
         WHERE resource_kind = 'memory'
           AND resource_id = NEW.memory_id
           AND grant_state = 'active';
        IF NEW.visibility <> 'private' THEN
            NEW.visibility := 'private';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER trg_revoke_grants_on_tombstone
    BEFORE UPDATE OF tombstoned_at ON proxima_core.memories
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.revoke_grants_on_tombstone();

-- ---------------------------------------------------------------------------
-- Hard-erase -> delete orphan grants
-- ---------------------------------------------------------------------------
-- Fact hard-erase DELETEs memory rows rather than tombstoning. Access is already
-- denied live-first by the existence check, but this keeps access_grants clean.
CREATE OR REPLACE FUNCTION proxima_core.delete_orphan_grants()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    DELETE FROM proxima_core.access_grants
     WHERE resource_kind = 'memory'
       AND resource_id = OLD.memory_id;
    RETURN OLD;
END;
$$;

CREATE TRIGGER trg_delete_orphan_grants
    AFTER DELETE ON proxima_core.memories
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.delete_orphan_grants();
