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
-- prior version's text, NULL included. The database enforces that carry rule
-- for every writer: `goal_text_carried` below refuses a version whose state
-- differs from its predecessor's and whose text is not the predecessor's, so a
-- writer that omits the column (an older process) cannot drop a text.
--
-- Additive and idempotent: ADD COLUMN IF NOT EXISTS skips the column and its
-- CHECK together on a second run, the backfill only writes rows whose text is
-- still NULL, and the function and trigger are CREATE OR REPLACE. An empty
-- database has no rows to visit.

ALTER TABLE proxima_core.goal
    ADD COLUMN IF NOT EXISTS text text
        CONSTRAINT goal_text_nonblank_chk
            CHECK (text IS NULL OR length(btrim(text, E' \t\n\r\x0b\f')) > 0);

-- Backfill. A version's text is the text of the latest declaration among the
-- rows of its own series at or before it that carries a non-blank one: a
-- transition or an achievement declares no text, so its version inherits, and
-- a later modify never reaches back to an earlier version. The text sits at a
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
-- `goal` is append-only, and `goal_append_only` refuses every UPDATE. The one
-- statement that is allowed to fill the new column runs with that guard off.
-- sqlx runs a migration in one transaction and DISABLE TRIGGER takes ACCESS
-- EXCLUSIVE, so no concurrent write sees the gap.
ALTER TABLE proxima_core.goal DISABLE TRIGGER goal_append_only;

WITH declared AS (
    -- The text each version's own declaration carries, or NULL.
    SELECT g.handle,
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
    -- next one in the same series; `run` counts the declared texts so far.
    SELECT handle, t, text,
           count(text) OVER (PARTITION BY handle ORDER BY t) AS run
      FROM declared
),
inherited AS (
    -- The first row of a run is the one that declares the text.
    SELECT t,
           first_value(text) OVER (PARTITION BY handle, run ORDER BY t) AS text
      FROM runs
)
UPDATE proxima_core.goal g
   SET text = i.text
  FROM inherited i
 WHERE i.t = g.t
   AND i.text IS NOT NULL
   AND g.text IS NULL;

ALTER TABLE proxima_core.goal ENABLE TRIGGER goal_append_only;

-- The carry rule. A version whose state differs from its predecessor's keeps
-- the predecessor's text exactly, NULL included; a version that keeps the
-- state (a modify) is free to change it. The first version of a series has no
-- predecessor and is free.
--
-- The predecessor is the series' last version, and a new version joins the
-- series after it: a t before the last one is refused (the last one's own t
-- is a duplicate, which the primary key refuses as one). Otherwise a
-- version slipped between two others would pass against its own neighbour
-- and leave the later one with a predecessor whose text it does not carry.
-- A Goal verb names the version it succeeds, and the head compare-and-set
-- refuses it when that is no longer the head, so a verb meets this only if the
-- clock stepped back; a raw writer that names no prior (write_goal) or picks
-- its own t is held to the series order here instead of misordering it.
--
-- Written the way `goal_no_later_after_terminal` guards the other series rule:
-- a BEFORE INSERT row trigger on `goal` raising 23514, an invoker routine. The
-- predecessor is a row of the inserted row's own owner, and the owner write
-- policy covers SELECT as well (FOR ALL), so every scope that may insert the
-- row sees it; no platform bridge is needed. The search_path is pinned like
-- the other routines since 0019.
--
-- One insert per series at a time. The trigger first locks the series'
-- goal_head row (FOR UPDATE), the lock every Goal writer already holds before
-- it inserts, and which exists for every goal row (goal.handle references it).
-- A writer that holds it takes it again for free; a raw insert waits for the
-- other writers and they for it. The predecessor read that follows is a new
-- statement, so under READ COMMITTED it sees the version the previous holder
-- committed, and two concurrent inserts can no longer both judge themselves
-- against the same predecessor.
CREATE OR REPLACE FUNCTION proxima_core.goal_text_carried()
RETURNS trigger
LANGUAGE plpgsql
SET search_path = pg_catalog, proxima_core, pg_temp
AS $$
DECLARE
    prior_t uuid;
    prior_state proxima_core.goal_state;
    prior_text text;
BEGIN
    PERFORM 1
       FROM proxima_core.goal_head h
      WHERE h.handle = NEW.handle
        FOR UPDATE;
    SELECT g.t, g.state, g.text
      INTO prior_t, prior_state, prior_text
      FROM proxima_core.goal g
     WHERE g.handle = NEW.handle
     ORDER BY g.t DESC
     LIMIT 1;
    IF FOUND AND prior_t > NEW.t THEN
        RAISE EXCEPTION
            'goal_text_carried: a version joins its series after the last version'
            USING ERRCODE = '23514';
    END IF;
    IF FOUND
       AND prior_state IS DISTINCT FROM NEW.state
       AND NEW.text IS DISTINCT FROM prior_text
    THEN
        RAISE EXCEPTION
            'goal_text_carried: a version whose state differs from its predecessor keeps the predecessor text exactly'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE TRIGGER goal_text_carried
    BEFORE INSERT ON proxima_core.goal
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.goal_text_carried();
