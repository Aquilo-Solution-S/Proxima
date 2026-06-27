-- 0005_group_ownership_access.sql
-- Group-ownership graph access model. Access leaves entity rows: group_membership
-- (roles, person->group) + entity_owner (reachability, entity->principal). A User
-- principal is its own singleton owner. World is a reserved Group constant.
-- See docs/superpowers/specs/2026-06-27-group-ownership-access-model-design.md

CREATE TYPE proxima_core.membership_relation AS ENUM ('admin', 'editor', 'viewer', 'ingest');

-- Roles: a person (User) belongs to a multi-person Group with a relation.
-- Multiple rows per (group, member) are allowed (a user may be both editor and
-- ingest); resolution UNIONs them. World membership is implicit (never a row).
CREATE TABLE proxima_core.group_membership (
    group_id        uuid NOT NULL,                              -- a Group principal
    member_user_id  uuid NOT NULL,                              -- a User principal
    relation        proxima_core.membership_relation NOT NULL,
    granted_by      uuid NOT NULL,                              -- personality_instance_id (audit)
    created_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, member_user_id, relation),
    -- World is viewer-only-implicit: it is never a membership group (closes the
    -- "World can never be in S_write" invariant at the schema, not just resolution).
    CONSTRAINT group_membership_not_world_chk
        CHECK (group_id <> '00000000-0000-0000-0000-000000000001')
);
CREATE INDEX idx_group_membership_member
    ON proxima_core.group_membership (member_user_id);

-- Reachability: which principals can reach an entity. Exactly one is_home row
-- (the single write owner). Other rows are read-only shares; a World row = public.
CREATE TABLE proxima_core.entity_owner (
    entity_id              uuid NOT NULL,
    owner_principal_kind   proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id     uuid NOT NULL,
    is_home                boolean NOT NULL DEFAULT false,
    granted_by             uuid,                                -- personality_instance_id (audit); NULL for user/external-authored entities (e.g. goals)
    created_at             timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (entity_id, owner_principal_kind, owner_principal_id),
    -- World is read-only: never a home owner.
    CONSTRAINT entity_owner_world_not_home_chk
        CHECK (NOT (is_home AND owner_principal_kind = 'Group'
                    AND owner_principal_id = '00000000-0000-0000-0000-000000000001'))
);
-- read path: "entities reachable by owner in S"
CREATE INDEX idx_entity_owner_by_owner
    ON proxima_core.entity_owner (owner_principal_kind, owner_principal_id, entity_id);
-- "who can reach this entity" + home lookup
CREATE INDEX idx_entity_owner_by_entity
    ON proxima_core.entity_owner (entity_id, is_home);
-- at-most-one home row per entity (exactly-one is the write-path invariant: create
-- always inserts a home row; unshare refuses to remove an is_home row, Task 6.1).
CREATE UNIQUE INDEX uq_entity_owner_home
    ON proxima_core.entity_owner (entity_id) WHERE is_home;

-- Tombstone (memories) -> drop reachability (entity leaves every surface incl. marketplace).
CREATE OR REPLACE FUNCTION proxima_core.drop_entity_owner_on_tombstone()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.tombstoned_at IS NOT NULL AND OLD.tombstoned_at IS NULL THEN
        DELETE FROM proxima_core.entity_owner WHERE entity_id = NEW.memory_id;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER trg_drop_entity_owner_on_tombstone
    BEFORE UPDATE OF tombstoned_at ON proxima_core.memories
    FOR EACH ROW EXECUTE FUNCTION proxima_core.drop_entity_owner_on_tombstone();

-- Hard-erase of a memory OR a goal -> drop its reachability rows. Goals have no
-- tombstoned_at (state machine + supersession), so erase is their only cleanup hook.
CREATE OR REPLACE FUNCTION proxima_core.drop_entity_owner_on_memory_erase()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM proxima_core.entity_owner WHERE entity_id = OLD.memory_id;
    RETURN OLD;
END;
$$;
CREATE TRIGGER trg_drop_entity_owner_on_memory_erase
    AFTER DELETE ON proxima_core.memories
    FOR EACH ROW EXECUTE FUNCTION proxima_core.drop_entity_owner_on_memory_erase();

CREATE OR REPLACE FUNCTION proxima_core.drop_entity_owner_on_goal_erase()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM proxima_core.entity_owner WHERE entity_id = OLD.goal_id;
    RETURN OLD;
END;
$$;
CREATE TRIGGER trg_drop_entity_owner_on_goal_erase
    AFTER DELETE ON proxima_core.goals
    FOR EACH ROW EXECUTE FUNCTION proxima_core.drop_entity_owner_on_goal_erase();

-- Backfill: every existing entity gets one is_home row from its current owner.
-- granted_by is the authoring personality where known, else NULL (no nil-UUID stand-in).
INSERT INTO proxima_core.entity_owner
    (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
SELECT m.memory_id, m.owner_principal_kind, m.owner_principal_id, true, m.personality_instance_id
  FROM proxima_core.memories m
 WHERE m.tombstoned_at IS NULL;
INSERT INTO proxima_core.entity_owner
    (entity_id, owner_principal_kind, owner_principal_id, is_home, granted_by)
SELECT g.goal_id, g.owner_principal_kind, g.owner_principal_id, true, g.personality_instance_id
  FROM proxima_core.goals g
ON CONFLICT DO NOTHING;
