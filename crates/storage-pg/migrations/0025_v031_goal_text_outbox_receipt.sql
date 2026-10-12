-- v0.0.31: a Goal version stores its prompt text, a published outbox record no
-- longer needs a broker receipt, and a goal write has a change Fact
-- (`core/goal-change-v1`, the last section).
--
-- `core_goal set` requires a text (the Goal's prompt), but the Goal row only
-- carried the title: the text lived in the replay declaration, which is request
-- bookkeeping, not a read model. `proxima_core.goal.text` is the text of that
-- version. NULL is "unknown": a version no declaration can speak for (a Goal
-- written before v0.0.13 recorded declarations) or a hand-written row. It is
-- never blank, and the column never changes after the row is written (the
-- table is append-only).
--
-- Blank has one definition here: nothing is left after removing ASCII
-- whitespace, that is space, tab, line feed, carriage return, vertical tab
-- (0x0B) and form feed. E-strings have no \v escape, so 0x0B is written \x0b.
-- The CHECK and the backfill below use the same expression. The Rust side
-- (`check_trimmed_len`, Unicode White_Space) refuses a superset of it, so
-- Rust never writes a text the CHECK refuses.
--
-- A write that gives a text (create, modify, a decomposed child) stores it. A
-- write that only moves the lifecycle (transition, achieve) stores exactly its
-- prior version's text, NULL included. The Goal write verbs do the carry. A
-- foreign key keeps a series to its head's owner: a version of another owner
-- cannot join it.
--
-- Additive and idempotent: ADD COLUMN IF NOT EXISTS skips the column and its
-- CHECK together on a second run, the backfill only writes rows whose text is
-- still NULL, and the unique constraint, the foreign key and the outbox check
-- are dropped and added again. An empty database has no rows to visit.

ALTER TABLE proxima_core.goal
    ADD COLUMN IF NOT EXISTS text text
        CONSTRAINT goal_text_nonblank_chk
            CHECK (text IS NULL OR length(btrim(text, E' \t\n\r\x0b\f')) > 0);

-- Backfill. A version's text is the text of the latest declaration among the
-- rows of its own series and its own owner at or before it that carries a
-- non-blank one: a transition or an achievement declares no text, so its
-- version inherits, and a later modify never reaches back to an earlier
-- version. The text sits at a
-- fixed place of each declaration envelope (`command_declaration` in
-- storage-pg `goal_write/replay.rs`: `{version, verb, ...body}`):
--
--   verb 'create'           declaration #> '{draft,text}'
--   verb 'modify'           declaration #> '{replacement,text}'
--   verb 'decompose_child'  declaration #> '{payload,text}'
--
-- 'transition' and 'achieve' have no text. A row with no such declaration
-- before it stays NULL; nothing is guessed from the title.
--
-- The windows partition by owner as well as handle. A Proxima writer keeps a
-- series to its head's owner, but before this version the database did not:
-- a raw insert could join another owner's handle (from 0025 the foreign key
-- below refuses such a row). Partitioning by owner keeps one owner's text out
-- of another owner's row, so that row's export and read never show it.
--
-- `goal` is append-only, and `goal_append_only` refuses every UPDATE. The one
-- statement that is allowed to fill the new column runs with that guard off.
-- sqlx runs a migration in one transaction and DISABLE TRIGGER takes ACCESS
-- EXCLUSIVE, so no concurrent write sees the gap.
ALTER TABLE proxima_core.goal DISABLE TRIGGER goal_append_only;

WITH declared AS (
    -- The text each version's own declaration carries, or NULL.
    SELECT g.handle,
           g.owner_id,
           g.t,
           CASE
               WHEN jsonb_typeof(d.declaration #> p.path) = 'string'
                AND length(btrim(d.declaration #>> p.path, E' \t\n\r\x0b\f')) > 0
               THEN d.declaration #>> p.path
           END AS text
      FROM proxima_core.goal g
      LEFT JOIN proxima_core.goal_replay_declaration d ON d.goal_t = g.t
      LEFT JOIN LATERAL (
          SELECT CASE d.declaration ->> 'verb'
                     WHEN 'create'          THEN ARRAY['draft', 'text']
                     WHEN 'modify'          THEN ARRAY['replacement', 'text']
                     WHEN 'decompose_child' THEN ARRAY['payload', 'text']
                 END AS path
      ) p ON true
),
runs AS (
    -- A run starts at each version that declares a text and lasts until the
    -- next one in the same series and owner; `run` counts the declared texts
    -- so far.
    SELECT handle, owner_id, t, text,
           count(text) OVER (PARTITION BY handle, owner_id ORDER BY t) AS run
      FROM declared
),
inherited AS (
    -- The first row of a run is the one that declares the text.
    SELECT t,
           first_value(text) OVER (PARTITION BY handle, owner_id, run ORDER BY t) AS text
      FROM runs
)
UPDATE proxima_core.goal g
   SET text = i.text
  FROM inherited i
 WHERE i.t = g.t
   AND i.text IS NOT NULL
   AND g.text IS NULL;

ALTER TABLE proxima_core.goal ENABLE TRIGGER goal_append_only;

-- A series belongs to its head's owner. A foreign key checks its target
-- without row-level security, so a row of another owner cannot join the series
-- whatever scope writes it. NOT VALID: a row an earlier raw insert put into
-- another owner's series stays; every new row is checked. Postgres has no
-- ADD CONSTRAINT IF NOT EXISTS, so both constraints are dropped (the foreign
-- key first, it depends on the unique one) and added again.
ALTER TABLE proxima_core.goal DROP CONSTRAINT IF EXISTS goal_owner_matches_head_fk;
ALTER TABLE proxima_core.goal_head DROP CONSTRAINT IF EXISTS goal_head_handle_owner_key;
ALTER TABLE proxima_core.goal_head
    ADD CONSTRAINT goal_head_handle_owner_key UNIQUE (handle, owner_id);
ALTER TABLE proxima_core.goal
    ADD CONSTRAINT goal_owner_matches_head_fk
    FOREIGN KEY (handle, owner_id) REFERENCES proxima_core.goal_head (handle, owner_id)
    NOT VALID;

-- Outbox: a published record needs no broker receipt. The outbox is drained in
-- process into a host intake, so a published record has no stream position to
-- name. `published_stream` and `published_seq` (0011) held the JetStream
-- PubAck; nothing writes them any more, and they stay, unwritten, because a
-- dropped column is not additive. `publication_outbox_published_chk` required
-- all three for a published row; it now ties `state = 'published'` to
-- `published_at` alone, in both directions. The old check let a record that
-- is not published carry `published_at` while a receipt field was NULL. No
-- Proxima write does that, but a hand-written row can, and the new check would
-- refuse it and abort the upgrade, so its `published_at` is cleared first: a
-- record that is not published has no publication time. The captured event is
-- not touched. The ADD validates every row once.
UPDATE proxima_core.publication_outbox
   SET published_at = NULL
 WHERE state <> 'published'
   AND published_at IS NOT NULL;
ALTER TABLE proxima_core.publication_outbox
    DROP CONSTRAINT IF EXISTS publication_outbox_published_chk;
ALTER TABLE proxima_core.publication_outbox
    ADD CONSTRAINT publication_outbox_published_chk CHECK (
        (state = 'published') = (published_at IS NOT NULL)
    );

-- Goal changes: `core/goal-change-v1`, one Fact per goal write (#435).
--
-- Every goal write that inserts a goal row also writes one Fact of this
-- schema, in the same transaction, when the schema is listenable (the runtime
-- turns that on when it drains the outbox into an intake). The Fact is an
-- ordinary listenable Fact, so the outbox, publication_origin, owner RLS and
-- erase need nothing new; only its typed sidecar table is new:
--
--   series        the goal's series handle (goal.handle), the same on every
--                 version
--   goal_t        the goal version this write created (goal.t)
--   prior_t       the version it replaced; NULL when the write starts a series
--                 (a set, a decomposed child)
--   revision      the version's position in its series: the number of goal
--                 rows with this handle once the write's row is in, so 1 for
--                 a set and a decomposed child
--   state         the goal's state after the write
--   change_kind   what the write did
--   parent_t      the parent of a decomposed child, on that kind alone
--   pending       what a pause does with the work a scheduler queued for the
--                 goal, on that kind alone
--
-- No foreign key into goal: the Fact records what happened and follows the
-- memory it describes, not the goal, so a goal erase never has to reach it.
-- The table is append-only, like the other core sidecars that are written
-- once.
--
-- Additive and idempotent: the types are created only when missing, the table
-- with IF NOT EXISTS, the triggers with OR REPLACE or DROP first, the surface
-- row skips a conflict, and the RLS installer replaces the policies it makes.
-- A second run changes nothing.

DO $goal_change_types$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_type AS t JOIN pg_namespace AS n ON n.oid = t.typnamespace
         WHERE n.nspname = 'proxima_core' AND t.typname = 'goal_change_kind'
    ) THEN
        CREATE TYPE proxima_core.goal_change_kind AS ENUM (
            'set', 'modify', 'decompose_child', 'pause', 'resume', 'abandon', 'achieve'
        );
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_type AS t JOIN pg_namespace AS n ON n.oid = t.typnamespace
         WHERE n.nspname = 'proxima_core' AND t.typname = 'goal_pending_work'
    ) THEN
        CREATE TYPE proxima_core.goal_pending_work AS ENUM ('keep', 'discard');
    END IF;
END
$goal_change_types$;

CREATE TABLE IF NOT EXISTS proxima_core.goal_change_v1 (
    t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
    series uuid NOT NULL,
    goal_t uuid NOT NULL,
    prior_t uuid,
    revision integer NOT NULL CONSTRAINT goal_change_v1_revision_chk CHECK (revision >= 1),
    state proxima_core.goal_state NOT NULL,
    change_kind proxima_core.goal_change_kind NOT NULL,
    parent_t uuid,
    pending proxima_core.goal_pending_work,
    CONSTRAINT goal_change_v1_pending_chk
        CHECK ((change_kind = 'pause') = (pending IS NOT NULL)),
    CONSTRAINT goal_change_v1_parent_chk
        CHECK ((change_kind = 'decompose_child') = (parent_t IS NOT NULL))
);

-- flavor_surface is FORCE-RLS, writable only in platform scope (0014, 0017).
-- The migration runner binds that scope; the block binds it itself so that a
-- bare re-run of this file (a repair, a test) is a no-op rather than a policy
-- refusal, and restores the caller's scope afterwards.
DO $goal_change_surface$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
        ('proxima_core.goal_change_v1', 'core')
    ON CONFLICT (table_name) DO NOTHING;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
END
$goal_change_surface$;

CREATE OR REPLACE TRIGGER goal_change_v1_append_only
    BEFORE UPDATE ON proxima_core.goal_change_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.enforce_row_append_only();

-- The declaration-integrity triggers of the table (the 0002 and 0009 families:
-- a sidecar row needs a memory that declares it, a memory that declares the
-- table needs the row, and neither survives the other's delete or re-point).
-- GENERATED — see `crates/storage-pg/src/integrity.rs` and the
-- `generated_declaration_triggers_are_the_migration_text` and
-- `generated_presence_triggers_are_the_migration_text` pins. Edit the
-- generator, not this block.
CREATE OR REPLACE TRIGGER goal_change_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.goal_change_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

DROP TRIGGER IF EXISTS memory_declares_proxima_core_goal_change_v1 ON proxima_core.memory;
CREATE CONSTRAINT TRIGGER memory_declares_proxima_core_goal_change_v1
    AFTER INSERT OR UPDATE OF sidecar_tables ON proxima_core.memory
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('proxima_core.goal_change_v1' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION proxima_core.assert_declared_sidecar_present('proxima_core.goal_change_v1', 't');

DROP TRIGGER IF EXISTS goal_change_v1_declared_by_memory_on_delete ON proxima_core.goal_change_v1;
CREATE CONSTRAINT TRIGGER goal_change_v1_declared_by_memory_on_delete
    AFTER DELETE ON proxima_core.goal_change_v1
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_row_not_still_declared('proxima_core.goal_change_v1', 't');

CREATE OR REPLACE TRIGGER goal_change_v1_declared_by_memory_on_update
    BEFORE UPDATE OF t ON proxima_core.goal_change_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');

-- Owner RLS, as the other FK sidecars of a memory have it (class fk_parent:
-- read in an owner scope, write by the parent memory's kind). The whole-schema
-- install_owner_rls of 0023 classified the tables that existed then; this
-- one table is added through the building blocks of the per-table installer
-- (0024), whose public entry refuses proxima_core. The fixture lane that skips
-- the owner-RLS migrations (test_fixtures::OWNER_RLS_MIGRATION_VERSIONS) has
-- neither installer nor policies, so there is nothing to extend there.
DO $goal_change_rls$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM pg_catalog.pg_proc AS p
          JOIN pg_catalog.pg_namespace AS n ON n.oid = p.pronamespace
         WHERE n.nspname = 'proxima_core' AND p.proname = 'owner_rls_apply_class'
    ) THEN
        PERFORM proxima_core.owner_rls_apply_class(
            '0025', 'class fk_parent', 'proxima_core', 'goal_change_v1', 'fk_parent', NULL);
        PERFORM proxima_core.owner_rls_census_table('proxima_core', 'goal_change_v1');
    END IF;
END
$goal_change_rls$;
