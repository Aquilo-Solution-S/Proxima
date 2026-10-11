-- v0.0.31: a Goal version stores its prompt text.
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
-- still NULL, and the unique constraint and the foreign key are dropped and
-- added again. An empty database has no rows to visit.

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
