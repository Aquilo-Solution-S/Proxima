-- Migration 0005, additive over the frozen 0003 reference-integrity and 0004
-- goal-reference lanes.
--
-- Install the erased-target witness and lifecycle guards after the frozen goal-reference split.
-- Earlier cooled pin arrays are nullable because stubs created before their lanes
-- cannot prove which declarations the cold object carried. New forgets write
-- the exact arrays, so receipt replay never has to invent an empty set.
--
-- `0001_v008.sql` through `0004_v011_goal_refs.sql` are frozen. This
-- migration adds the erased-target witness and replaces trigger functions
-- without re-adding columns or rewriting an applied migration.

-- This migration must never start with two homes for one identity. The
-- witness is historical metadata, not a repair tool for an already-corrupt
-- database; failing before any DDL leaves the old lane untouched.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM (
              SELECT t FROM proxima_core.memory
              UNION ALL
              SELECT t FROM proxima_core.cooled
              UNION ALL
              SELECT t FROM proxima_core.goal
          ) ids
         GROUP BY t
        HAVING count(*) > 1
    ) THEN
        RAISE EXCEPTION
            'the erased-target witness refuses identity collisions across memory, cooled and goal';
    END IF;
END;
$$;


DO $$
BEGIN
    IF to_regtype('proxima_core.pin_target_kind') IS NULL THEN
        CREATE TYPE proxima_core.pin_target_kind AS ENUM (
            'fact',
            'abstraction',
            'perspective',
            'goal'
        );
    END IF;
END;
$$;

CREATE TABLE proxima_core.erased_pin_target (
    t uuid PRIMARY KEY,
    kind proxima_core.pin_target_kind NOT NULL
);

COMMENT ON TABLE proxima_core.erased_pin_target IS
'Internal historical identity witness for a hard-erased pin target. It carries no owner or readable payload and is excluded from export.';

-- One vocabulary for every lifecycle participant. The trigger and the Rust
-- paths use this exact key so a write and an erase cannot take crossed locks.
CREATE OR REPLACE FUNCTION proxima_core.lock_pin_targets(targets uuid[])
RETURNS void
LANGUAGE plpgsql
VOLATILE
AS $$
DECLARE
    target uuid;
BEGIN
    FOR target IN
        SELECT DISTINCT id
          FROM unnest(COALESCE(targets, '{}'::uuid[])) AS pins(id)
         WHERE id IS NOT NULL
         ORDER BY id
    LOOP
        PERFORM pg_advisory_xact_lock(
            hashtextextended('proxima-forget:' || target::text, 0)
        );
    END LOOP;
END;
$$;

-- Witness rows are written only by the declared target DELETE triggers below.
-- `pg_trigger_depth() >= 2` distinguishes their nested insert from a direct
-- INSERT, while the transaction-local marker keeps an unrelated trigger from
-- becoming a second writer. The schema owner remains a trusted boundary.
--
-- The marker carries the exact identity being witnessed, not a boolean. A
-- boolean that leaked — because the INSERT below raised before its reset ran —
-- would stay armed for the rest of the transaction and admit any nested
-- INSERT; an identity-scoped marker can only ever admit the one target whose
-- write already failed.
CREATE OR REPLACE FUNCTION proxima_core.assert_erased_pin_target_insert()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF pg_trigger_depth() < 2
       OR current_setting('proxima_core.erased_pin_target_writer', true)
              IS DISTINCT FROM NEW.t::text
    THEN
        RAISE EXCEPTION
            'erased_pin_target is written only by a target deletion trigger'
            USING ERRCODE = '42501';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM proxima_core.memory m
         WHERE m.t = NEW.t AND m.kind::text = NEW.kind::text
        UNION ALL
        SELECT 1 FROM proxima_core.cooled c
         WHERE c.t = NEW.t AND c.kind::text = NEW.kind::text
        UNION ALL
        SELECT 1 FROM proxima_core.goal g
         WHERE g.t = NEW.t AND NEW.kind = 'goal'
    ) THEN
        RAISE EXCEPTION
            'erased_pin_target % must match the live row being deleted', NEW.t
            USING ERRCODE = '23503';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.erased_pin_target_append_only()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'erased_pin_target is append-only'
        USING ERRCODE = '25006';
END;
$$;

CREATE TRIGGER erased_pin_target_insert_guard
    BEFORE INSERT ON proxima_core.erased_pin_target
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_erased_pin_target_insert();

CREATE TRIGGER erased_pin_target_append_only
    BEFORE UPDATE OR DELETE ON proxima_core.erased_pin_target
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.erased_pin_target_append_only();

-- The only witness writer. A same-kind witness is idempotent for a repeated
-- backstop delete; a different kind fails closed instead of concealing an
-- identity collision behind ON CONFLICT.
CREATE OR REPLACE FUNCTION proxima_core.record_erased_pin_target(
    target uuid,
    target_kind proxima_core.pin_target_kind
)
RETURNS void
LANGUAGE plpgsql
VOLATILE
AS $$
DECLARE
    existing_kind proxima_core.pin_target_kind;
BEGIN
    PERFORM proxima_core.lock_pin_targets(ARRAY[target]);
    SELECT kind INTO existing_kind
      FROM proxima_core.erased_pin_target
     WHERE t = target;
    IF FOUND THEN
        IF existing_kind <> target_kind THEN
            RAISE EXCEPTION
                'erased pin target % already records kind %, not %',
                target, existing_kind, target_kind
                USING ERRCODE = '23505';
        END IF;
        RETURN;
    END IF;

    PERFORM set_config('proxima_core.erased_pin_target_writer', target::text, true);
    INSERT INTO proxima_core.erased_pin_target (t, kind)
    VALUES (target, target_kind);
    PERFORM set_config('proxima_core.erased_pin_target_writer', '', true);
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.memory_erase_witness()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM proxima_core.lock_pin_targets(ARRAY[OLD.t]);
    -- Memory -> cooled is forget, not hard erase. Hydration's cooled row is
    -- likewise present when it deletes the cooled half, so neither transition
    -- manufactures a historical witness.
    IF NOT EXISTS (SELECT 1 FROM proxima_core.cooled WHERE t = OLD.t) THEN
        PERFORM proxima_core.record_erased_pin_target(
            OLD.t, OLD.kind::text::proxima_core.pin_target_kind
        );
    END IF;
    RETURN OLD;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.cooled_erase_witness()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM proxima_core.lock_pin_targets(ARRAY[OLD.t]);
    -- Cooled -> Memory is hydration, not hard erase.
    IF NOT EXISTS (SELECT 1 FROM proxima_core.memory WHERE t = OLD.t) THEN
        PERFORM proxima_core.record_erased_pin_target(
            OLD.t, OLD.kind::text::proxima_core.pin_target_kind
        );
    END IF;
    RETURN OLD;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.goal_erase_witness()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM proxima_core.lock_pin_targets(ARRAY[OLD.t]);
    PERFORM proxima_core.record_erased_pin_target(OLD.t, 'goal');
    RETURN OLD;
END;
$$;

CREATE TRIGGER memory_erased_pin_target
    BEFORE DELETE ON proxima_core.memory
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.memory_erase_witness();

CREATE TRIGGER cooled_erased_pin_target
    BEFORE DELETE ON proxima_core.cooled
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.cooled_erase_witness();

CREATE TRIGGER goal_erased_pin_target
    BEFORE DELETE ON proxima_core.goal
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.goal_erase_witness();

-- Cooled rows are forget snapshots. New snapshots must be seals of the hot
-- row they replace; old nullable-array stubs remain readable for migration.
-- The exact cooled tuple is the database restoration seal: Rust's raw cold
-- decoder may propose pins, but only this trusted schema-owner backstop can
-- admit them. No application flag or session setting opts into restoration.
CREATE OR REPLACE FUNCTION proxima_core.cooled_identity_seal()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    PERFORM proxima_core.lock_pin_targets(ARRAY[NEW.t]);
    IF EXISTS (SELECT 1 FROM proxima_core.erased_pin_target WHERE t = NEW.t)
       OR EXISTS (SELECT 1 FROM proxima_core.goal WHERE t = NEW.t)
    THEN
        RAISE EXCEPTION 'cooled target % collides with a goal or erased target', NEW.t
            USING ERRCODE = '23505';
    END IF;

    -- Let the column CHECK name malformed arrays. Legacy cooled rows may have
    -- NULL declaration arrays; new rows carry all three arrays.
    IF (NEW.origins IS NOT NULL AND array_position(NEW.origins, NULL) IS NOT NULL)
       OR (NEW.refs IS NOT NULL AND array_position(NEW.refs, NULL) IS NOT NULL)
       OR (NEW.goal_refs IS NOT NULL AND array_position(NEW.goal_refs, NULL) IS NOT NULL)
    THEN
        RETURN NEW;
    END IF;

    IF NOT EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.t = NEW.t
           AND m.handle = NEW.handle
           AND m.owner_id = NEW.owner_id
           AND m.kind = NEW.kind
           AND m.blob_id IS NOT DISTINCT FROM NEW.blob_id
           AND m.content_id IS NOT DISTINCT FROM NEW.content_id
           AND m.source_id IS NOT DISTINCT FROM NEW.source_id
           AND m.ingest_key IS NOT DISTINCT FROM NEW.ingest_key
           AND m.origins IS NOT DISTINCT FROM NEW.origins
           AND m.refs IS NOT DISTINCT FROM NEW.refs
           AND m.goal_refs IS NOT DISTINCT FROM NEW.goal_refs
    ) THEN
        RAISE EXCEPTION 'cooled row % does not seal its hot Memory', NEW.t
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER cooled_identity_seal
    BEFORE INSERT ON proxima_core.cooled
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.cooled_identity_seal();

CREATE OR REPLACE FUNCTION proxima_core.cooled_append_only()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.t IS DISTINCT FROM OLD.t
       OR NEW.handle IS DISTINCT FROM OLD.handle
       OR NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.object_key IS DISTINCT FROM OLD.object_key
       OR NEW.source_id IS DISTINCT FROM OLD.source_id
       OR NEW.ingest_key IS DISTINCT FROM OLD.ingest_key
       OR NEW.origins IS DISTINCT FROM OLD.origins
       OR NEW.refs IS DISTINCT FROM OLD.refs
       OR NEW.goal_refs IS DISTINCT FROM OLD.goal_refs
       OR NEW.cooled_at IS DISTINCT FROM OLD.cooled_at
    THEN
        RAISE EXCEPTION
            'cooled is frozen except owner_id, blob_id and content_id remaps'
            USING ERRCODE = '25006';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER cooled_append_only
    BEFORE UPDATE ON proxima_core.cooled
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.cooled_append_only();

-- The SQL backstop follows the same set-first order as the Rust forget path:
-- source and every hot non-Fact depender take the lifecycle advisory before
-- any depender row lock used by the grounding check.
CREATE OR REPLACE FUNCTION proxima_core.cooled_forget_grounding() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    dependent_ids uuid[];
BEGIN
    IF NEW.kind = 'fact' THEN
        RETURN NEW;
    END IF;
    SELECT COALESCE(array_agg(m.t ORDER BY m.t), '{}'::uuid[])
      INTO dependent_ids
      FROM proxima_core.memory m
     WHERE m.kind <> 'fact'
       AND m.t <> NEW.t
       AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t]);
    PERFORM proxima_core.lock_pin_targets(ARRAY[NEW.t] || dependent_ids);
    IF EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.kind <> 'fact'
           AND m.t <> NEW.t
           AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t])
           AND NOT (m.t = ANY (dependent_ids))
    ) THEN
        RAISE EXCEPTION
            'forget depender footprint grew after lifecycle lock acquisition'
            USING ERRCODE = '40001';
    END IF;
    -- Lock dependers only after the complete lifecycle set is held.
    PERFORM 1
      FROM proxima_core.memory m
     WHERE m.kind <> 'fact'
       AND m.t <> NEW.t
       AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t])
     ORDER BY m.t
     FOR UPDATE;
    IF EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.kind <> 'fact'
           AND m.t <> NEW.t
           AND (m.origins @> ARRAY[NEW.t] OR m.refs @> ARRAY[NEW.t])
           AND NOT proxima_core.pins_have_grounding_support(
                 m.origins || m.refs, NEW.t, NEW.kind
               )
    ) THEN
        RAISE EXCEPTION 'forget would leave an ungrounded memory'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

-- Goal and wake declarations are also target admissions. Rust validates their
-- live endpoint kinds; this database guard closes the witness hole when a
-- caller reaches the tables without the engine.
CREATE OR REPLACE FUNCTION proxima_core.goal_pin_target_checks()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    pin uuid;
BEGIN
    PERFORM proxima_core.lock_pin_targets(
        array_remove(
            ARRAY[NEW.t, NEW.close_fact_t, NEW.assignment_t, NEW.write_act_t],
            NULL
        ) || NEW.dependency_t || NEW.evidence_t
    );
    SELECT e.t INTO pin
      FROM proxima_core.erased_pin_target e
     WHERE e.t = ANY (
         array_remove(
             ARRAY[NEW.close_fact_t, NEW.assignment_t, NEW.write_act_t], NULL
         ) || NEW.dependency_t || NEW.evidence_t
     )
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'goal declaration names erased target %', pin
            USING ERRCODE = '23503';
    END IF;
    IF EXISTS (SELECT 1 FROM proxima_core.erased_pin_target WHERE t = NEW.t)
       OR EXISTS (SELECT 1 FROM proxima_core.memory WHERE t = NEW.t)
       OR EXISTS (SELECT 1 FROM proxima_core.cooled WHERE t = NEW.t)
    THEN
        RAISE EXCEPTION 'goal t % collides with an existing entity', NEW.t
            USING ERRCODE = '23505';
    END IF;
    RETURN NEW;
END;
$$;

CREATE OR REPLACE FUNCTION proxima_core.wake_pin_target_checks()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    pin uuid;
BEGIN
    PERFORM proxima_core.lock_pin_targets(
        array_remove(ARRAY[NEW.trigger_t], NULL) || NEW.hard_memory_t
    );
    SELECT e.t INTO pin
      FROM proxima_core.erased_pin_target e
     WHERE e.t = ANY (
         array_remove(ARRAY[NEW.trigger_t], NULL) || NEW.hard_memory_t
     )
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'wake configuration names erased target %', pin
            USING ERRCODE = '23503';
    END IF;
    IF NEW.trigger_t IS NOT NULL
       AND NOT EXISTS (SELECT 1 FROM proxima_core.memory WHERE t = NEW.trigger_t)
    THEN
        RAISE EXCEPTION 'wake trigger memory does not exist'
            USING ERRCODE = '23503';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM unnest(NEW.hard_memory_t) AS h(t)
         WHERE NOT EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.t = h.t)
    ) THEN
        RAISE EXCEPTION 'wake hard context memory does not exist'
            USING ERRCODE = '23503';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER goal_pin_target_checks
    BEFORE INSERT ON proxima_core.goal
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.goal_pin_target_checks();

CREATE TRIGGER wake_pin_target_checks
    BEFORE INSERT OR UPDATE OF trigger_t, hard_memory_t ON proxima_core.wake_config
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.wake_pin_target_checks();

-- Origins and references have different target vocabularies. Keep the
-- existence checks set-based and lock hot targets before checking them: a
-- concurrent erase must either wait for this admission or win before it, not
-- disappear a target between the check and insertion of the source row.
CREATE OR REPLACE FUNCTION proxima_core.memory_pin_checks()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    pin uuid;
    pin_handle uuid;
    historical_restore boolean;
BEGIN
    PERFORM proxima_core.lock_pin_targets(
        ARRAY[NEW.t] || NEW.origins || NEW.refs || NEW.goal_refs
    );

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

    IF EXISTS (SELECT 1 FROM proxima_core.goal WHERE t = NEW.t)
       OR EXISTS (SELECT 1 FROM proxima_core.erased_pin_target WHERE t = NEW.t)
    THEN
        RAISE EXCEPTION 'memory t % is already a Goal or erased target', NEW.t
            USING ERRCODE = '23505';
    END IF;

    SELECT EXISTS (
        SELECT 1
          FROM proxima_core.cooled c
         WHERE c.t = NEW.t
           AND c.handle = NEW.handle
           AND c.owner_id = NEW.owner_id
           AND c.kind = NEW.kind
           AND c.source_id IS NOT DISTINCT FROM NEW.source_id
           AND c.ingest_key IS NOT DISTINCT FROM NEW.ingest_key
           AND c.blob_id IS NOT DISTINCT FROM NEW.blob_id
           AND c.content_id IS NOT DISTINCT FROM NEW.content_id
           AND c.origins IS NOT NULL
           AND c.refs IS NOT NULL
           AND c.goal_refs IS NOT NULL
           AND c.origins = NEW.origins
           AND c.refs = NEW.refs
           AND c.goal_refs = NEW.goal_refs
    ) INTO historical_restore;

    -- A sealed cooled row may only be reinserted with the exact identity it
    -- carried. This prevents a direct INSERT from laundering a new row
    -- through a cooled identity.
    IF EXISTS (
        SELECT 1
          FROM proxima_core.cooled c
         WHERE c.t = NEW.t
           AND NOT (
               c.handle = NEW.handle
               AND c.owner_id = NEW.owner_id
               AND c.kind = NEW.kind
               AND c.source_id IS NOT DISTINCT FROM NEW.source_id
               AND c.ingest_key IS NOT DISTINCT FROM NEW.ingest_key
               AND c.blob_id IS NOT DISTINCT FROM NEW.blob_id
               AND c.content_id IS NOT DISTINCT FROM NEW.content_id
           )
    ) THEN
        RAISE EXCEPTION 'memory insert % does not match its cooled identity seal', NEW.t
            USING ERRCODE = '23514';
    END IF;

    -- Nullable arrays are legacy rows. A row with no declaration arrays is
    -- history from before migration 0003; any partial declaration is malformed and must not
    -- fall onto the live-target path, where it could launder a changed pin.
    IF EXISTS (
        SELECT 1
          FROM proxima_core.cooled c
         WHERE c.t = NEW.t
           AND (
               c.origins IS NOT NULL
               OR c.refs IS NOT NULL
               OR c.goal_refs IS NOT NULL
           )
           AND NOT (
               c.origins IS NOT DISTINCT FROM NEW.origins
               AND c.refs IS NOT DISTINCT FROM NEW.refs
               AND c.goal_refs IS NOT DISTINCT FROM NEW.goal_refs
           )
    ) THEN
        RAISE EXCEPTION 'memory insert % does not match its cooled restoration seal', NEW.t
            USING ERRCODE = '23514';
    END IF;

    IF NEW.kind = 'fact'
       AND NEW.origins = '{}'
       AND NEW.refs = '{}'
       AND NEW.goal_refs = '{}'
    THEN
        RETURN NEW;
    END IF;

    -- Goal references never provide F/A/P grounding. A historical restore is
    -- the sole exception: its exact cooled seal already proves the original
    -- admitted declaration and its erased witness preserves target kind.
    IF NEW.kind <> 'fact' AND NOT historical_restore
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

    -- Origins are always Memory targets. A historical restore may use only a
    -- matching non-Goal witness; a Goal witness must never satisfy layering.
    SELECT p.id INTO pin
      FROM unnest(NEW.origins) AS p(id)
      LEFT JOIN proxima_core.memory m ON m.t = p.id
      LEFT JOIN proxima_core.cooled c ON c.t = p.id
     LEFT JOIN proxima_core.erased_pin_target e ON e.t = p.id
     WHERE m.t IS NULL AND c.t IS NULL
       AND (
           e.t IS NULL
           OR NOT (
               historical_restore
               AND e.kind IN ('fact', 'abstraction', 'perspective')
           )
       )
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'origin pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    -- `refs` carries only Memory targets after the 0004 split.
    SELECT p.id INTO pin
      FROM unnest(NEW.refs) AS p(id)
      LEFT JOIN proxima_core.memory m ON m.t = p.id
      LEFT JOIN proxima_core.cooled c ON c.t = p.id
     LEFT JOIN proxima_core.erased_pin_target e ON e.t = p.id
     WHERE m.t IS NULL AND c.t IS NULL
       AND (
           e.t IS NULL
           OR NOT (
               historical_restore
               AND e.kind IN ('fact', 'abstraction', 'perspective')
           )
       )
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'reference pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    -- `goal_refs` carries only Goal targets, including a retained Goal
    -- witness for an exact historical restore.
    SELECT p.id INTO pin
      FROM unnest(NEW.goal_refs) AS p(id)
      LEFT JOIN proxima_core.goal g ON g.t = p.id
      LEFT JOIN proxima_core.erased_pin_target e
        ON e.t = p.id AND e.kind = 'goal'
     WHERE g.t IS NULL
       AND (e.t IS NULL OR NOT historical_restore)
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'goal reference pin % does not exist as a Goal', pin
            USING ERRCODE = '23503';
    END IF;

    IF NOT historical_restore THEN
        SELECT m.handle INTO pin_handle
          FROM proxima_core.memory m
          JOIN proxima_core.closed_handle c ON c.handle = m.handle
         WHERE m.t = ANY (NEW.origins || NEW.refs)
         LIMIT 1;
        IF FOUND THEN
            RAISE EXCEPTION 'closed_handle: no new pin to %', pin_handle
                USING ERRCODE = '23514';
        END IF;
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
               AND NOT (historical_restore AND EXISTS (
                       SELECT 1 FROM proxima_core.erased_pin_target e
                        WHERE e.t = o.id AND e.kind IN ('fact', 'abstraction')
                   ))
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
               AND NOT (historical_restore AND EXISTS (
                       SELECT 1 FROM proxima_core.erased_pin_target e
                        WHERE e.t = o.id AND e.kind = 'abstraction'
                   ))
        ) THEN
            RAISE EXCEPTION 'perspective origins must be abstraction t'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;
