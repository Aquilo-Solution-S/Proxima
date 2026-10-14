-- v0.0.30: an owner is one owner id, a UUID.
--
-- A resolved owner is always a group, and a user is the personal group whose
-- only member is that user (docs/lean/Causa/Owner.lean). Rows already key on
-- `owner_id` alone; this drops the redundant kind from the three tables that
-- still stored it beside the id, and makes `proxima_core.owners.kind` the one
-- place the kind lives. Re-running the file changes nothing.

-- 1. The registry pair is unique, so a membership can reference it.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'proxima_core.owners'::regclass
           AND conname = 'owners_owner_id_kind_key'
    ) THEN
        ALTER TABLE proxima_core.owners
            ADD CONSTRAINT owners_owner_id_kind_key UNIQUE (owner_id, kind);
    END IF;
END
$$;

-- 2. A group is registered when it is created. Groups created before this
--    release were registered on their first data write, or not at all:
--    register every group a membership names.
INSERT INTO proxima_core.owners (owner_id, kind)
SELECT DISTINCT m.group_id, 'group'::proxima_core.owner_kind
  FROM proxima_core.group_memberships m
ON CONFLICT (owner_id) DO NOTHING;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM proxima_core.group_memberships m
          JOIN proxima_core.owners o ON o.owner_id = m.group_id
         WHERE o.kind <> 'group'
    ) THEN
        RAISE EXCEPTION 'v033 owner id: a group_memberships row names an owner registered as personal; a personal owner has no members, so remove that membership before upgrading';
    END IF;
END
$$;

-- 3. A membership's group must be a registered group: the database refuses
--    a membership under a user's id.
ALTER TABLE proxima_core.group_memberships
    ADD COLUMN IF NOT EXISTS group_kind proxima_core.owner_kind NOT NULL DEFAULT 'group';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'proxima_core.group_memberships'::regclass
           AND conname = 'group_memberships_group_kind_chk'
    ) THEN
        ALTER TABLE proxima_core.group_memberships
            ADD CONSTRAINT group_memberships_group_kind_chk CHECK (group_kind = 'group');
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
         WHERE conrelid = 'proxima_core.group_memberships'::regclass
           AND conname = 'group_memberships_group_registered_fk'
    ) THEN
        ALTER TABLE proxima_core.group_memberships
            ADD CONSTRAINT group_memberships_group_registered_fk
            FOREIGN KEY (group_id, group_kind)
            REFERENCES proxima_core.owners (owner_id, kind);
    END IF;
END
$$;

-- 4. delegated_authority_grants: the grant names its owner by id.
ALTER TABLE proxima_core.delegated_authority_grants
    DROP COLUMN IF EXISTS owner_kind;

-- 5. source_cursors: keyed (owner_id, source). One id is one owner, so the
--    old key held at most one row per pair; keep the newest if not.
DELETE FROM proxima_core.source_cursors stale
 USING proxima_core.source_cursors newer
 WHERE stale.owner_id = newer.owner_id
   AND stale.source = newer.source
   AND stale.ctid <> newer.ctid
   AND (stale.updated_at, stale.ctid) < (newer.updated_at, newer.ctid);

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM information_schema.columns
         WHERE table_schema = 'proxima_core'
           AND table_name = 'source_cursors'
           AND column_name = 'owner_kind'
    ) THEN
        ALTER TABLE proxima_core.source_cursors DROP CONSTRAINT source_cursors_pkey;
        ALTER TABLE proxima_core.source_cursors DROP COLUMN owner_kind;
        ALTER TABLE proxima_core.source_cursors
            ADD CONSTRAINT source_cursors_pkey PRIMARY KEY (owner_id, source);
    END IF;
END
$$;

-- 6. publication_origin: the original owner is its id.
DROP INDEX IF EXISTS proxima_core.publication_origin_owner_source_idx;
ALTER TABLE proxima_core.publication_origin
    DROP CONSTRAINT IF EXISTS publication_origin_owner_kind_chk;
ALTER TABLE proxima_core.publication_origin
    DROP COLUMN IF EXISTS original_owner_kind;
CREATE INDEX IF NOT EXISTS publication_origin_owner_source_idx
    ON proxima_core.publication_origin (original_owner_id, source_id, t);
