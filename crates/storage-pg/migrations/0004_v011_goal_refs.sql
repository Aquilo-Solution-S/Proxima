-- v0.0.11, additive over the frozen v0.0.8 baseline, the v0.0.9 trigger lane
-- and the v0.0.10 reference-integrity lane.
--
-- `memory.refs` carried both Memory and Goal targets in one uuid[], so the
-- endpoint kind was erased at persistence and every reader re-derived it by
-- probing the Goal spine. Split the column: `refs` is Memory-only, `goal_refs`
-- is Goal-only. Storage now mirrors `PinNode`, which already held that shape.
--
-- NON-DISCLOSURE INVARIANT. The split narrows which spine a reader probes. It
-- must never change what a reader is told about a target it cannot read: an
-- unreadable Memory reference and an unreadable Goal reference both project
-- `Redacted`, indistinguishably. The discriminant is a probe hint, never an
-- authorization or projection input.
--
-- `0001_v008.sql`, `0002_v009_declaration_triggers.sql` and
-- `0003_v010_reference_integrity.sql` are frozen. This file adds the columns,
-- splits the existing rows, and replaces two function bodies; the baseline
-- checksums remain valid for live databases.

ALTER TABLE proxima_core.memory
    ADD COLUMN goal_refs uuid[] NOT NULL DEFAULT '{}',
    ADD CONSTRAINT memory_goal_refs_no_null_chk
        CHECK (array_position(goal_refs, NULL) IS NULL);

CREATE INDEX memory_goal_refs_gin
    ON proxima_core.memory USING gin (goal_refs);

-- Cooled witnesses stay nullable for the same reason 0003 made them nullable:
-- a stub written before that migration cannot prove which declarations the
-- cold object carried, and inventing an empty set would bless a changed one.
ALTER TABLE proxima_core.cooled
    ADD COLUMN goal_refs uuid[],
    ADD CONSTRAINT cooled_goal_refs_no_null_chk
        CHECK (goal_refs IS NULL OR array_position(goal_refs, NULL) IS NULL);

-- Backfill. Unlike the cooled reference arrays 0003 had to leave NULL, this
-- split is exact and recoverable: a Goal id is decided by membership of the
-- goal spine, which is right here. Without it every pre-migration Goal
-- reference would read back as a Memory target, miss the visibility load, and
-- render `Redacted` — a silent correctness regression, not an upgrade wrinkle.
--
-- `memory` is append-only and `memory_owner_or_append_only` names `refs`
-- explicitly, so the one statement that is allowed to rewrite it runs with
-- that guard off. sqlx runs a migration in one transaction and DISABLE
-- TRIGGER takes ACCESS EXCLUSIVE, so no concurrent write sees the gap.
ALTER TABLE proxima_core.memory DISABLE TRIGGER memory_append_only;

UPDATE proxima_core.memory m SET
    goal_refs = ARRAY(
        SELECT u FROM unnest(m.refs) AS u
         WHERE EXISTS (SELECT 1 FROM proxima_core.goal g WHERE g.t = u)),
    refs = ARRAY(
        SELECT u FROM unnest(m.refs) AS u
         WHERE NOT EXISTS (SELECT 1 FROM proxima_core.goal g WHERE g.t = u))
 WHERE m.refs <> '{}'
   AND EXISTS (
           SELECT 1 FROM unnest(m.refs) AS u
            WHERE EXISTS (SELECT 1 FROM proxima_core.goal g WHERE g.t = u));

ALTER TABLE proxima_core.memory ENABLE TRIGGER memory_append_only;

-- `cooled` has no append-only UPDATE guard: its only trigger is the
-- BEFORE INSERT grounding check. A NULL `refs` stays unsplit and its
-- `goal_refs` stays NULL — the declaration was never persisted for those rows.
UPDATE proxima_core.cooled c SET
    goal_refs = ARRAY(
        SELECT u FROM unnest(c.refs) AS u
         WHERE EXISTS (SELECT 1 FROM proxima_core.goal g WHERE g.t = u)),
    refs = ARRAY(
        SELECT u FROM unnest(c.refs) AS u
         WHERE NOT EXISTS (SELECT 1 FROM proxima_core.goal g WHERE g.t = u))
 WHERE c.refs IS NOT NULL;

-- `goal_refs` joins the append-only set. Leaving it out would make the new
-- column the one mutable pin array on an otherwise append-only row.
CREATE OR REPLACE FUNCTION proxima_core.memory_owner_or_append_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.t IS DISTINCT FROM OLD.t
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.schema_id IS DISTINCT FROM OLD.schema_id
       OR NEW.source_id IS DISTINCT FROM OLD.source_id
       OR NEW.ingest_key IS DISTINCT FROM OLD.ingest_key
       OR NEW.origins IS DISTINCT FROM OLD.origins
       OR NEW.refs IS DISTINCT FROM OLD.refs
       OR NEW.goal_refs IS DISTINCT FROM OLD.goal_refs
       OR NEW.sidecar_tables IS DISTINCT FROM OLD.sidecar_tables THEN
        RAISE EXCEPTION 'append-only: % does not accept UPDATE', TG_TABLE_NAME
            USING ERRCODE = '25006';
    END IF;
    IF NEW.blob_id IS DISTINCT FROM OLD.blob_id
       AND NOT EXISTS (
               SELECT 1
                 FROM proxima_core.blob old_blob
                 JOIN proxima_core.blob new_blob
                   ON new_blob.schema_id = old_blob.schema_id
                  AND new_blob.content_hash = old_blob.content_hash
                WHERE old_blob.blob_id = OLD.blob_id
                  AND new_blob.blob_id = NEW.blob_id
           ) THEN
        RAISE EXCEPTION
            'append-only: %.blob_id may only be repointed at a blob row naming the same '
            'schema_id and content_hash', TG_TABLE_NAME
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

-- Each pin column is now checked against exactly one spine. A Goal id in
-- `refs` and a Memory id in `goal_refs` are both rejected by their own
-- existence check, so the column IS the type.
CREATE OR REPLACE FUNCTION proxima_core.memory_pin_checks() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    pin uuid;
    pin_handle uuid;
BEGIN
    IF NEW.kind = 'fact'
       AND NEW.origins = '{}' AND NEW.refs = '{}' AND NEW.goal_refs = '{}' THEN
        RETURN NEW;
    END IF;

    -- Every hot Memory target is locked in one global t order, then every Goal
    -- target in its own. Locking a spine in two phases would let two crossed
    -- declarations take M1 then M2 and M2 then M1 and deadlock before either
    -- can admit. Splitting the column does not change that: each spine is
    -- still entered once, in sorted order.
    IF NEW.origins <> '{}' OR NEW.refs <> '{}' THEN
        PERFORM 1
          FROM proxima_core.memory
         WHERE t = ANY (NEW.origins || NEW.refs)
         ORDER BY t
         FOR SHARE;
    END IF;
    IF NEW.goal_refs <> '{}' THEN
        PERFORM 1
          FROM proxima_core.goal
         WHERE t = ANY (NEW.goal_refs)
         ORDER BY t
         FOR SHARE;
    END IF;

    -- Grounding is unchanged, and deliberately does not see `goal_refs`: it
    -- asks membership of `memory`/`cooled`, which a Goal t never satisfied.
    -- Moving Goals out of `refs` is therefore semantically neutral here.
    IF NEW.kind <> 'fact'
       AND NOT proxima_core.pins_have_grounding_support(
             NEW.origins || NEW.refs, NULL, NULL
           )
    THEN
        RAISE EXCEPTION 'non-fact must pin a hot memory or a cooled fact'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.origins = '{}' AND NEW.refs = '{}' AND NEW.goal_refs = '{}' THEN
        RETURN NEW;
    END IF;

    SELECT p.id INTO pin
      FROM unnest(NEW.origins) AS p(id)
      LEFT JOIN proxima_core.memory m ON m.t = p.id
      LEFT JOIN proxima_core.cooled c ON c.t = p.id
     WHERE m.t IS NULL AND c.t IS NULL
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'origin pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    SELECT p.id INTO pin
      FROM unnest(NEW.refs) AS p(id)
      LEFT JOIN proxima_core.memory m ON m.t = p.id
      LEFT JOIN proxima_core.cooled c ON c.t = p.id
     WHERE m.t IS NULL AND c.t IS NULL
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'reference pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    SELECT p.id INTO pin
      FROM unnest(NEW.goal_refs) AS p(id)
      LEFT JOIN proxima_core.goal g ON g.t = p.id
     WHERE g.t IS NULL
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'goal reference pin % does not exist as a Goal', pin
            USING ERRCODE = '23503';
    END IF;

    SELECT m.handle INTO pin_handle
      FROM proxima_core.memory m
      JOIN proxima_core.closed_handle c ON c.handle = m.handle
     WHERE m.t = ANY (NEW.origins || NEW.refs)
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'closed_handle: no new pin to %', pin_handle
            USING ERRCODE = '23514';
    END IF;

    IF NEW.kind = 'abstraction' AND NEW.origins <> '{}' THEN
        IF EXISTS (
            SELECT 1
              FROM unnest(NEW.origins) AS o(id)
             WHERE NOT EXISTS (
                       SELECT 1 FROM proxima_core.memory m
                        WHERE m.t = o.id AND m.kind IN ('fact', 'abstraction')
                   )
               AND NOT EXISTS (
                       SELECT 1 FROM proxima_core.cooled c
                        WHERE c.t = o.id AND c.kind IN ('fact', 'abstraction')
                   )
        ) THEN
            RAISE EXCEPTION 'abstraction origins must be fact or abstraction t'
                USING ERRCODE = '23514';
        END IF;
    ELSIF NEW.kind = 'perspective' AND NEW.origins <> '{}' THEN
        IF EXISTS (
            SELECT 1
              FROM unnest(NEW.origins) AS o(id)
             WHERE NOT EXISTS (
                       SELECT 1 FROM proxima_core.memory m
                        WHERE m.t = o.id AND m.kind = 'abstraction'
                   )
               AND NOT EXISTS (
                       SELECT 1 FROM proxima_core.cooled c
                        WHERE c.t = o.id AND c.kind = 'abstraction'
                   )
        ) THEN
            RAISE EXCEPTION 'perspective origins must be abstraction t'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;
