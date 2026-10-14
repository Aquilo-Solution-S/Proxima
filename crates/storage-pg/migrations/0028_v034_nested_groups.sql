-- v0.0.31: positioned outbox records and materialized nested group access.
-- Forward migration: upgrades v0.0.30 in place, including removal of unused
-- record claim columns (as 0027_v033_owner_id.sql removed retired columns).
-- This is not a destructive baseline; captured envelopes remain immutable.
-- Stop older hosts first. Drain pre-v0.0.28 lanes with a configured consumer,
-- or retire their consumer through PublicationConsumerPort::retire (including
-- an unconfigured `default` consumer). Delivered records are preserved.
--
-- First operation: refuse before any schema change if a legacy lane remains.
DO $require_publication_position$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF EXISTS (
        SELECT 1
          FROM proxima_core.publication_delivery AS lane
          JOIN proxima_core.publication_outbox AS record ON record.t = lane.t
         WHERE lane.state <> 'published'
           AND ((convert_from(record.envelope, 'UTF8')::jsonb ->> 'subject') IS NULL
                OR (convert_from(record.envelope, 'UTF8')::jsonb ->> 'sequence') IS NULL)
    ) THEN
        RAISE EXCEPTION USING ERRCODE = '23514',
            MESSAGE = 'v0.0.31 requires pre-v0.0.28 outbox records to be settled: drain with a configured consumer, or retire the consumer (including default) through PublicationConsumerPort::retire; see docs/18-fact-outbox.md lanes';
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
END
$require_publication_position$;

DO $publication_record_state$
BEGIN
    IF to_regtype('proxima_core.publication_record_state') IS NULL THEN
        CREATE TYPE proxima_core.publication_record_state AS ENUM ('pending', 'published');
    END IF;
END
$publication_record_state$;

-- Lane claims keep publication_state. The record's aggregate has no claimed
-- variant; rebuild the state-dependent constraint/index for its narrower type.
ALTER TABLE proxima_core.publication_outbox
    DROP CONSTRAINT IF EXISTS publication_outbox_no_claim_chk,
    DROP CONSTRAINT IF EXISTS publication_outbox_claim_chk,
    DROP CONSTRAINT IF EXISTS publication_outbox_published_chk,
    DROP COLUMN IF EXISTS claim_token,
    DROP COLUMN IF EXISTS claimed_by,
    DROP COLUMN IF EXISTS lease_expires_at,
    DROP COLUMN IF EXISTS attempts,
    ALTER COLUMN state DROP DEFAULT;
DROP INDEX IF EXISTS proxima_core.publication_outbox_pending_idx;
ALTER TABLE proxima_core.publication_outbox
    ALTER COLUMN state TYPE proxima_core.publication_record_state
        USING state::text::proxima_core.publication_record_state,
    ALTER COLUMN state SET DEFAULT 'pending',
    ADD CONSTRAINT publication_outbox_published_chk CHECK (
        (state = 'published') = (published_at IS NOT NULL)
    );
CREATE INDEX publication_outbox_pending_idx
    ON proxima_core.publication_outbox (t) WHERE state <> 'published';

-- Group membership is a forest and access is materialized.
-- Membership rows remain the input.  The two tables below are maintained only
-- by the trigger functions in this file; hosts read the projection.

CREATE TABLE IF NOT EXISTS proxima_core.group_edges (
    child_group_id uuid PRIMARY KEY,
    parent_group_id uuid NOT NULL,
    relation proxima_core.membership_relation NOT NULL DEFAULT 'viewer',
    child_group_kind proxima_core.owner_kind NOT NULL DEFAULT 'group',
    parent_group_kind proxima_core.owner_kind NOT NULL DEFAULT 'group',
    CONSTRAINT group_edges_child_kind_chk CHECK (child_group_kind = 'group'),
    CONSTRAINT group_edges_parent_kind_chk CHECK (parent_group_kind = 'group'),
    CONSTRAINT group_edges_child_group_fk FOREIGN KEY (child_group_id, child_group_kind)
        REFERENCES proxima_core.owners (owner_id, kind)
        DEFERRABLE INITIALLY IMMEDIATE,
    CONSTRAINT group_edges_parent_group_fk FOREIGN KEY (parent_group_id, parent_group_kind)
        REFERENCES proxima_core.owners (owner_id, kind)
        DEFERRABLE INITIALLY IMMEDIATE
);

CREATE INDEX IF NOT EXISTS group_edges_parent_idx
    ON proxima_core.group_edges (parent_group_id);

CREATE TABLE IF NOT EXISTS proxima_core.group_role_projection (
    member_user_id uuid NOT NULL,
    group_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
    read_ceiling proxima_core.access_ceiling NOT NULL,
    write_ceiling proxima_core.access_ceiling NOT NULL,
    manage boolean NOT NULL,
    CONSTRAINT group_role_projection_role_chk CHECK (
        (write_ceiling = 'none' OR write_ceiling = 'fact' AND read_ceiling IN ('fact','abstraction','perspective','goal')
         OR write_ceiling = 'abstraction' AND read_ceiling IN ('abstraction','perspective','goal')
         OR write_ceiling = 'perspective' AND read_ceiling IN ('perspective','goal')
         OR write_ceiling = 'goal' AND read_ceiling = 'goal')
    ),
    PRIMARY KEY (member_user_id, group_id)
);

CREATE INDEX IF NOT EXISTS group_role_projection_group_idx
    ON proxima_core.group_role_projection (group_id, member_user_id);

-- Host forms enter the same lock path through this trigger gateway.  The
-- BEFORE trigger consumes the request, so the table never stores a lock act.
CREATE TABLE IF NOT EXISTS proxima_core.group_lock_request (
    request_id uuid PRIMARY KEY,
    left_group_id uuid NOT NULL,
    right_group_id uuid,
    member_is_group boolean NOT NULL DEFAULT false
);
ALTER TABLE proxima_core.group_lock_request
    ADD COLUMN IF NOT EXISTS member_is_group boolean NOT NULL DEFAULT false;

CREATE OR REPLACE FUNCTION proxima_core.group_tree_root(p_group uuid)
RETURNS uuid
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core, pg_temp
AS $group_tree_root$
    WITH RECURSIVE parents(group_id, depth) AS (
        SELECT p_group, 0
      UNION ALL
        SELECT e.parent_group_id, p.depth + 1
          FROM parents p JOIN proxima_core.group_edges e
            ON e.child_group_id = p.group_id
    )
    SELECT group_id FROM parents ORDER BY depth DESC LIMIT 1
$group_tree_root$;

-- The lock key is the root id, not the first group named by a caller.  The
-- loop is required because a root can change between the read and the lock.
CREATE OR REPLACE FUNCTION proxima_core.lock_group_tree(p_group uuid)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
    SET search_path = pg_catalog, proxima_core, pg_temp
AS $lock_group_tree$
DECLARE
    before_root uuid;
    after_root uuid;
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING
            ERRCODE = '25001',
            MESSAGE = 'nested group membership writes require READ COMMITTED';
    END IF;
    LOOP
        WITH RECURSIVE parents(group_id, depth) AS (
            SELECT p_group, 0
          UNION ALL
            SELECT e.parent_group_id, p.depth + 1
              FROM parents p JOIN proxima_core.group_edges e
                ON e.child_group_id = p.group_id
        )
        SELECT group_id INTO before_root FROM parents ORDER BY depth DESC LIMIT 1;
        before_root := COALESCE(before_root, p_group);
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-group-tree:' || before_root::text, 0));
        WITH RECURSIVE parents(group_id, depth) AS (
            SELECT p_group, 0
          UNION ALL
            SELECT e.parent_group_id, p.depth + 1
              FROM parents p JOIN proxima_core.group_edges e
                ON e.child_group_id = p.group_id
        )
        SELECT group_id INTO after_root FROM parents ORDER BY depth DESC LIMIT 1;
        after_root := COALESCE(after_root, p_group);
        EXIT WHEN before_root = after_root;
    END LOOP;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
END
$lock_group_tree$;

CREATE OR REPLACE FUNCTION proxima_core.lock_group_trees(p_left uuid, p_right uuid)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
    SET search_path = pg_catalog, proxima_core, pg_temp
AS $lock_group_trees$
DECLARE
    left_root uuid;
    right_root uuid;
    checked_left uuid;
    checked_right uuid;
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION USING ERRCODE = '25001',
            MESSAGE = 'nested group membership writes require READ COMMITTED';
    END IF;
    LOOP
        left_root := proxima_core.group_tree_root(p_left);
        right_root := proxima_core.group_tree_root(p_right);
        left_root := COALESCE(left_root, p_left);
        right_root := COALESCE(right_root, p_right);
        IF left_root <= right_root THEN
            PERFORM pg_advisory_xact_lock(hashtextextended('proxima-group-tree:' || left_root::text, 0));
            IF left_root <> right_root THEN
                PERFORM pg_advisory_xact_lock(hashtextextended('proxima-group-tree:' || right_root::text, 0));
            END IF;
        ELSE
            PERFORM pg_advisory_xact_lock(hashtextextended('proxima-group-tree:' || right_root::text, 0));
            PERFORM pg_advisory_xact_lock(hashtextextended('proxima-group-tree:' || left_root::text, 0));
        END IF;
        checked_left := proxima_core.group_tree_root(p_left);
        checked_right := proxima_core.group_tree_root(p_right);
        EXIT WHEN checked_left = left_root AND checked_right = right_root;
    END LOOP;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
END
$lock_group_trees$;

CREATE OR REPLACE FUNCTION proxima_core.group_lock_request_trigger()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core, pg_temp
AS $group_lock_request_trigger$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF TG_OP = 'INSERT' THEN
        IF NEW.right_group_id IS NULL THEN
            PERFORM proxima_core.lock_group_tree(NEW.left_group_id);
        ELSE
            PERFORM proxima_core.lock_group_trees(NEW.left_group_id, NEW.right_group_id);
        END IF;
        IF NEW.right_group_id IS NULL THEN
            NEW.member_is_group := false;
        ELSE
            IF NEW.left_group_id <= NEW.right_group_id THEN
                PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.left_group_id::text, 0));
                PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.right_group_id::text, 0));
            ELSE
                PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.right_group_id::text, 0));
                PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.left_group_id::text, 0));
            END IF;
            SELECT EXISTS (
                SELECT 1 FROM proxima_core.owners
                 WHERE owner_id = NEW.right_group_id AND kind = 'group'
            ) INTO NEW.member_is_group;
        END IF;
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;
    RETURN OLD;
END
$group_lock_request_trigger$;

DROP TRIGGER IF EXISTS group_lock_request_trigger ON proxima_core.group_lock_request;
CREATE TRIGGER group_lock_request_trigger
BEFORE INSERT ON proxima_core.group_lock_request
FOR EACH ROW EXECUTE FUNCTION proxima_core.group_lock_request_trigger();
CREATE OR REPLACE FUNCTION proxima_core.group_lock_request_cleanup()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core, pg_temp
AS $group_lock_request_cleanup$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    DELETE FROM proxima_core.group_lock_request WHERE request_id = NEW.request_id;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NULL;
END
$group_lock_request_cleanup$;
DROP TRIGGER IF EXISTS group_lock_request_cleanup ON proxima_core.group_lock_request;
CREATE TRIGGER group_lock_request_cleanup
AFTER INSERT ON proxima_core.group_lock_request
FOR EACH ROW EXECUTE FUNCTION proxima_core.group_lock_request_cleanup();
GRANT INSERT, SELECT ON proxima_core.group_lock_request TO PUBLIC;

-- Fold every path for one set of people.  The recursive CTE is deliberately
-- expressed in lattice coordinates so its min/max/boolean operations are the
-- SQL form of Role::meet and Role::join.
CREATE OR REPLACE FUNCTION proxima_core.rebuild_group_role_projection(p_users uuid[])
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
    SET search_path = pg_catalog, proxima_core, pg_temp
AS $rebuild_group_role_projection$
DECLARE
    user_row record;
    user_id uuid;
    previous_scope text := current_setting('app.proxima_scope', true);
    previous_maintenance text := current_setting('proxima.projection_maintenance', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM set_config('proxima.projection_maintenance', 'on', true);
    FOR user_row IN
        SELECT DISTINCT u AS user_id
          FROM unnest(COALESCE(p_users, '{}')) AS u
         ORDER BY u
    LOOP
        user_id := user_row.user_id;
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-group-projection:' || user_id::text, 0));
        DELETE FROM proxima_core.group_role_projection WHERE member_user_id = user_id;
        INSERT INTO proxima_core.group_role_projection
        (member_user_id, group_id, read_ceiling, write_ceiling, manage)
    WITH RECURSIVE paths(member_user_id, group_id, read_rank, write_rank, manages) AS (
        SELECT m.member_user_id, m.group_id,
               CASE m.relation WHEN 'admin' THEN 4 WHEN 'editor' THEN 4 WHEN 'viewer' THEN 4 ELSE 1 END,
               CASE m.relation WHEN 'admin' THEN 4 WHEN 'editor' THEN 3 WHEN 'ingest' THEN 1 ELSE 0 END,
               m.relation = 'admin'
          FROM proxima_core.group_memberships m
         WHERE m.member_user_id = user_id
        UNION ALL
        SELECT p.member_user_id, e.parent_group_id,
               LEAST(p.read_rank, CASE e.relation WHEN 'admin' THEN 4 WHEN 'editor' THEN 4 WHEN 'viewer' THEN 4 ELSE 1 END),
               LEAST(p.write_rank, CASE e.relation WHEN 'admin' THEN 4 WHEN 'editor' THEN 3 WHEN 'ingest' THEN 1 ELSE 0 END),
               p.manages AND e.relation = 'admin'
          FROM paths p JOIN proxima_core.group_edges e ON e.child_group_id = p.group_id
    ), folded AS (
        SELECT member_user_id, group_id, max(read_rank) AS read_rank,
               max(write_rank) AS write_rank, bool_or(manages) AS manages
          FROM paths GROUP BY member_user_id, group_id
    )
    SELECT member_user_id, group_id,
           (ARRAY['none','fact','abstraction','perspective','goal'])[read_rank + 1]::proxima_core.access_ceiling,
           (ARRAY['none','fact','abstraction','perspective','goal'])[write_rank + 1]::proxima_core.access_ceiling,
           manages
      FROM folded;
    END LOOP;
    PERFORM set_config('proxima.projection_maintenance', COALESCE(previous_maintenance, ''), true);
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
END
$rebuild_group_role_projection$;

CREATE OR REPLACE FUNCTION proxima_core.group_projection_write_guard()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core, pg_temp
AS $group_projection_write_guard$
BEGIN
    IF current_setting('proxima.projection_maintenance', true) IS DISTINCT FROM 'on'
       OR pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION 'group_role_projection is maintained by membership triggers';
    END IF;
    RETURN COALESCE(NEW, OLD);
END
$group_projection_write_guard$;

CREATE OR REPLACE FUNCTION proxima_core.group_truncate_guard()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core, pg_temp
AS $group_truncate_guard$
BEGIN
    RAISE EXCEPTION '% cannot be truncated; membership projection requires row triggers', TG_TABLE_NAME;
END
$group_truncate_guard$;

CREATE OR REPLACE FUNCTION proxima_core.group_membership_before_write()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
    SET search_path = pg_catalog, proxima_core, pg_temp
AS $group_membership_before_write$
DECLARE
    member_kind proxima_core.owner_kind;
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    PERFORM proxima_core.lock_group_tree(COALESCE(NEW.group_id, OLD.group_id));
    IF TG_OP = 'DELETE' THEN
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-owner-kind:' || OLD.member_user_id::text, 0));
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN OLD;
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.group_id, NEW.member_user_id) <> (OLD.group_id, OLD.member_user_id) THEN
        RAISE EXCEPTION 'membership identity is immutable';
    END IF;
    PERFORM pg_advisory_xact_lock(
        hashtextextended('proxima-owner-kind:' || NEW.member_user_id::text, 0));
    SELECT kind INTO member_kind FROM proxima_core.owners WHERE owner_id = NEW.member_user_id;
    IF member_kind = 'group' THEN
        RAISE EXCEPTION 'owner % is a group; use the group edge write', NEW.member_user_id;
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END
$group_membership_before_write$;

CREATE OR REPLACE FUNCTION proxima_core.group_edge_before_write()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
    SET search_path = pg_catalog, proxima_core, pg_temp
AS $group_edge_before_write$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF TG_OP = 'DELETE' THEN
        PERFORM proxima_core.lock_group_tree(OLD.child_group_id);
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-owner-kind:' || OLD.child_group_id::text, 0));
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN OLD;
    END IF;
    IF TG_OP = 'UPDATE' AND (NEW.child_group_id, NEW.parent_group_id) <> (OLD.child_group_id, OLD.parent_group_id) THEN
        RAISE EXCEPTION 'group edge identity is immutable';
    END IF;
    PERFORM proxima_core.lock_group_trees(NEW.child_group_id, NEW.parent_group_id);
    IF NEW.child_group_id <= NEW.parent_group_id THEN
        PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.child_group_id::text, 0));
        PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.parent_group_id::text, 0));
    ELSE
        PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.parent_group_id::text, 0));
        PERFORM pg_advisory_xact_lock(hashtextextended('proxima-owner-kind:' || NEW.child_group_id::text, 0));
    END IF;
    IF NEW.child_group_id = NEW.parent_group_id THEN
        RAISE EXCEPTION 'a group cannot contain itself';
    END IF;
    IF EXISTS (
        WITH RECURSIVE descendants(group_id) AS (
            SELECT NEW.parent_group_id
          UNION ALL SELECT e.parent_group_id FROM descendants d
            JOIN proxima_core.group_edges e ON e.child_group_id = d.group_id
        ) SELECT 1 FROM descendants WHERE group_id = NEW.child_group_id
    ) THEN
        RAISE EXCEPTION 'group edge would create a cycle';
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END
$group_edge_before_write$;

CREATE OR REPLACE FUNCTION proxima_core.group_projection_after_write()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
    SET search_path = pg_catalog, proxima_core, pg_temp
AS $group_projection_after_write$
DECLARE
    users uuid[];
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    IF TG_TABLE_NAME = 'group_memberships' THEN
        users := ARRAY[COALESCE(NEW.member_user_id, OLD.member_user_id)];
    ELSE
        WITH RECURSIVE descendants(group_id) AS (
            SELECT COALESCE(NEW.child_group_id, OLD.child_group_id)
          UNION ALL SELECT e.child_group_id FROM descendants d
            JOIN proxima_core.group_edges e ON e.parent_group_id = d.group_id
        )
        SELECT COALESCE(array_agg(DISTINCT m.member_user_id), '{}') INTO users
          FROM descendants d JOIN proxima_core.group_memberships m ON m.group_id = d.group_id;
    END IF;
    IF cardinality(users) > 0 THEN PERFORM proxima_core.rebuild_group_role_projection(users); END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NULL;
END
$group_projection_after_write$;

DROP TRIGGER IF EXISTS group_membership_before_write ON proxima_core.group_memberships;
CREATE TRIGGER group_membership_before_write
BEFORE INSERT OR UPDATE OR DELETE ON proxima_core.group_memberships
FOR EACH ROW EXECUTE FUNCTION proxima_core.group_membership_before_write();
DROP TRIGGER IF EXISTS group_edge_before_write ON proxima_core.group_edges;
CREATE TRIGGER group_edge_before_write
BEFORE INSERT OR UPDATE OR DELETE ON proxima_core.group_edges
FOR EACH ROW EXECUTE FUNCTION proxima_core.group_edge_before_write();
DROP TRIGGER IF EXISTS group_membership_projection ON proxima_core.group_memberships;
CREATE TRIGGER group_membership_projection
AFTER INSERT OR UPDATE OR DELETE ON proxima_core.group_memberships
FOR EACH ROW EXECUTE FUNCTION proxima_core.group_projection_after_write();
DROP TRIGGER IF EXISTS group_edge_projection ON proxima_core.group_edges;
CREATE TRIGGER group_edge_projection
AFTER INSERT OR UPDATE OR DELETE ON proxima_core.group_edges
FOR EACH ROW EXECUTE FUNCTION proxima_core.group_projection_after_write();
DROP TRIGGER IF EXISTS group_projection_write_guard ON proxima_core.group_role_projection;
CREATE TRIGGER group_projection_write_guard
BEFORE INSERT OR UPDATE OR DELETE ON proxima_core.group_role_projection
FOR EACH ROW EXECUTE FUNCTION proxima_core.group_projection_write_guard();
DROP TRIGGER IF EXISTS group_membership_truncate_guard ON proxima_core.group_memberships;
CREATE TRIGGER group_membership_truncate_guard
BEFORE TRUNCATE ON proxima_core.group_memberships
FOR EACH STATEMENT EXECUTE FUNCTION proxima_core.group_truncate_guard();
DROP TRIGGER IF EXISTS group_edge_truncate_guard ON proxima_core.group_edges;
CREATE TRIGGER group_edge_truncate_guard
BEFORE TRUNCATE ON proxima_core.group_edges
FOR EACH STATEMENT EXECUTE FUNCTION proxima_core.group_truncate_guard();
DROP TRIGGER IF EXISTS group_projection_truncate_guard ON proxima_core.group_role_projection;
CREATE TRIGGER group_projection_truncate_guard
BEFORE TRUNCATE ON proxima_core.group_role_projection
FOR EACH STATEMENT EXECUTE FUNCTION proxima_core.group_truncate_guard();

-- Registration is the other ingress for the kind invariant.
CREATE OR REPLACE FUNCTION proxima_core.owner_group_kind_guard()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core, pg_temp
AS $owner_group_kind_guard$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
    existing_kind proxima_core.owner_kind;
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    -- Personal registration is the v0.0.30 first-seen path.  The owners
    -- primary key and immutable-owner trigger already serialize it; taking a
    -- per-owner advisory lock here would add one lock to every memory write.
    IF NEW.kind = 'personal' THEN
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;
    IF NEW.kind = 'group' THEN
        PERFORM proxima_core.lock_group_tree(NEW.owner_id);
    END IF;
    PERFORM pg_advisory_xact_lock(
        hashtextextended('proxima-owner-kind:' || NEW.owner_id::text, 0));
    SELECT kind INTO existing_kind
      FROM proxima_core.owners
     WHERE owner_id = NEW.owner_id;
    IF FOUND THEN
        -- Registration is idempotent.  The owners immutability trigger owns
        -- UPDATE rejection; INSERT ... ON CONFLICT DO NOTHING must be able
        -- to propose either kind without changing an existing registration.
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;
    IF NEW.kind = 'group' AND EXISTS (
        SELECT 1 FROM proxima_core.group_memberships WHERE member_user_id = NEW.owner_id
    ) THEN
        RAISE EXCEPTION 'owner % cannot become a group while it has person memberships', NEW.owner_id;
    END IF;
    IF NEW.kind = 'personal' AND EXISTS (
        SELECT 1 FROM proxima_core.group_edges WHERE child_group_id = NEW.owner_id
    ) THEN
        RAISE EXCEPTION 'owner % cannot become a person while it is a group member', NEW.owner_id;
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END
$owner_group_kind_guard$;

DROP TRIGGER IF EXISTS owner_group_kind_guard ON proxima_core.owners;
CREATE TRIGGER owner_group_kind_guard
BEFORE INSERT OR UPDATE ON proxima_core.owners
FOR EACH ROW EXECUTE FUNCTION proxima_core.owner_group_kind_guard();

-- These tables have the group-membership policy class: group scope may read
-- the relevant rows, manage scope may write edges, and only the table owner
-- may use the platform policy.  The projection itself is trigger-maintained.
DO $nested_group_rls$
DECLARE
    owner_role name;
BEGIN
    IF to_regclass('proxima_core.owner_rls_epoch') IS NULL THEN
        RETURN;
    END IF;
    SELECT pg_get_userbyid(c.relowner) INTO owner_role
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'proxima_core' AND c.relname = 'group_edges';
    EXECUTE 'ALTER TABLE proxima_core.group_edges ENABLE ROW LEVEL SECURITY';
    EXECUTE 'ALTER TABLE proxima_core.group_edges FORCE ROW LEVEL SECURITY';
    EXECUTE 'DROP POLICY IF EXISTS proxima_owner_read ON proxima_core.group_edges';
    EXECUTE 'DROP POLICY IF EXISTS proxima_owner_write ON proxima_core.group_edges';
    EXECUTE 'DROP POLICY IF EXISTS proxima_platform ON proxima_core.group_edges';
    EXECUTE 'CREATE POLICY proxima_owner_read ON proxima_core.group_edges FOR SELECT TO PUBLIC USING (child_group_id = ANY(COALESCE(NULLIF(current_setting(''app.owner'', true), '''')::uuid[], ''{}''::uuid[])) OR parent_group_id = ANY(COALESCE(NULLIF(current_setting(''app.owner'', true), '''')::uuid[], ''{}''::uuid[])))';
    EXECUTE 'CREATE POLICY proxima_owner_write ON proxima_core.group_edges FOR ALL TO PUBLIC USING (parent_group_id = ANY(COALESCE(NULLIF(current_setting(''app.manage_owner'', true), '''')::uuid[], ''{}''::uuid[])) OR child_group_id = ANY(COALESCE(NULLIF(current_setting(''app.manage_owner'', true), '''')::uuid[], ''{}''::uuid[]))) WITH CHECK (parent_group_id = ANY(COALESCE(NULLIF(current_setting(''app.manage_owner'', true), '''')::uuid[], ''{}''::uuid[])) OR child_group_id = ANY(COALESCE(NULLIF(current_setting(''app.manage_owner'', true), '''')::uuid[], ''{}''::uuid[])))';
    EXECUTE format('CREATE POLICY proxima_platform ON proxima_core.group_edges FOR ALL TO %I USING (current_setting(''app.proxima_scope'', true) = ''platform'') WITH CHECK (current_setting(''app.proxima_scope'', true) = ''platform'')', owner_role);

    SELECT pg_get_userbyid(c.relowner) INTO owner_role
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'proxima_core' AND c.relname = 'group_role_projection';
    EXECUTE 'ALTER TABLE proxima_core.group_role_projection ENABLE ROW LEVEL SECURITY';
    EXECUTE 'ALTER TABLE proxima_core.group_role_projection FORCE ROW LEVEL SECURITY';
    EXECUTE 'DROP POLICY IF EXISTS proxima_owner_read ON proxima_core.group_role_projection';
    EXECUTE 'DROP POLICY IF EXISTS proxima_owner_write ON proxima_core.group_role_projection';
    EXECUTE 'DROP POLICY IF EXISTS proxima_platform ON proxima_core.group_role_projection';
    EXECUTE 'CREATE POLICY proxima_owner_read ON proxima_core.group_role_projection FOR SELECT TO PUBLIC USING (group_id = ANY(COALESCE(NULLIF(current_setting(''app.owner'', true), '''')::uuid[], ''{}''::uuid[])))';
    EXECUTE 'CREATE POLICY proxima_owner_write ON proxima_core.group_role_projection FOR ALL TO PUBLIC USING (group_id = ANY(COALESCE(NULLIF(current_setting(''app.manage_owner'', true), '''')::uuid[], ''{}''::uuid[]))) WITH CHECK (group_id = ANY(COALESCE(NULLIF(current_setting(''app.manage_owner'', true), '''')::uuid[], ''{}''::uuid[])))';
    EXECUTE format('CREATE POLICY proxima_platform ON proxima_core.group_role_projection FOR ALL TO %I USING (current_setting(''app.proxima_scope'', true) = ''platform'') WITH CHECK (current_setting(''app.proxima_scope'', true) = ''platform'')', owner_role);

    SELECT pg_get_userbyid(c.relowner) INTO owner_role
      FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname = 'proxima_core' AND c.relname = 'group_lock_request';
    EXECUTE 'ALTER TABLE proxima_core.group_lock_request ENABLE ROW LEVEL SECURITY';
    EXECUTE 'ALTER TABLE proxima_core.group_lock_request FORCE ROW LEVEL SECURITY';
    EXECUTE 'DROP POLICY IF EXISTS proxima_owner_read ON proxima_core.group_lock_request';
    EXECUTE 'DROP POLICY IF EXISTS proxima_owner_write ON proxima_core.group_lock_request';
    EXECUTE 'DROP POLICY IF EXISTS proxima_platform ON proxima_core.group_lock_request';
    EXECUTE 'CREATE POLICY proxima_owner_read ON proxima_core.group_lock_request FOR SELECT TO PUBLIC USING (request_id IS NOT NULL)';
    EXECUTE 'CREATE POLICY proxima_owner_write ON proxima_core.group_lock_request FOR ALL TO PUBLIC USING (left_group_id = ANY(COALESCE(NULLIF(current_setting(''app.manage_owner'', true), '''')::uuid[], ''{}''::uuid[])) OR right_group_id = ANY(COALESCE(NULLIF(current_setting(''app.manage_owner'', true), '''')::uuid[], ''{}''::uuid[]))) WITH CHECK (left_group_id = ANY(COALESCE(NULLIF(current_setting(''app.manage_owner'', true), '''')::uuid[], ''{}''::uuid[])) OR right_group_id = ANY(COALESCE(NULLIF(current_setting(''app.manage_owner'', true), '''')::uuid[], ''{}''::uuid[])))';
    EXECUTE format('CREATE POLICY proxima_platform ON proxima_core.group_lock_request FOR ALL TO %I USING (current_setting(''app.proxima_scope'', true) = ''platform'') WITH CHECK (current_setting(''app.proxima_scope'', true) = ''platform'')', owner_role);
END
$nested_group_rls$;

GRANT SELECT ON proxima_core.group_edges, proxima_core.group_role_projection TO PUBLIC;
GRANT INSERT ON proxima_core.group_lock_request TO PUBLIC;

-- Seed upgraded databases after the trigger guard exists.
DROP TRIGGER IF EXISTS group_projection_write_guard ON proxima_core.group_role_projection;
SELECT proxima_core.rebuild_group_role_projection(
    COALESCE((SELECT array_agg(DISTINCT member_user_id) FROM proxima_core.group_memberships), '{}'));
CREATE TRIGGER group_projection_write_guard
BEFORE INSERT OR UPDATE OR DELETE ON proxima_core.group_role_projection
FOR EACH ROW EXECUTE FUNCTION proxima_core.group_projection_write_guard();

REVOKE ALL ON FUNCTION proxima_core.group_tree_root(uuid) FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.rebuild_group_role_projection(uuid[]) FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.group_projection_write_guard() FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.group_truncate_guard() FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.group_membership_before_write() FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.group_edge_before_write() FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.group_projection_after_write() FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.owner_group_kind_guard() FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.group_lock_request_cleanup() FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.group_lock_request_trigger() FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.lock_group_tree(uuid) FROM PUBLIC;
REVOKE ALL ON FUNCTION proxima_core.lock_group_trees(uuid, uuid) FROM PUBLIC;
