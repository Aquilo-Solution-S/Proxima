-- v0.0.10, additive over the frozen v0.0.8 baseline and v0.0.9 trigger lane.
--
-- Memory references may point at a Goal, while origins remain Memory-only.
-- Cooled pin arrays are nullable because stubs created before this migration
-- cannot prove which declarations the cold object carried. New forgets write
-- the exact arrays, so receipt replay never has to invent an empty set.
--
-- `0001_v008.sql` and `0002_v009_declaration_triggers.sql` are frozen. This
-- file replaces only the pin-admission trigger and adds the nullable cooled
-- witnesses; the baseline's checksums remain valid for live databases.

ALTER TABLE proxima_core.cooled
    ADD COLUMN origins uuid[],
    ADD COLUMN refs uuid[];

ALTER TABLE proxima_core.cooled
    ADD CONSTRAINT cooled_origins_no_null_chk
        CHECK (origins IS NULL OR array_position(origins, NULL) IS NULL),
    ADD CONSTRAINT cooled_refs_no_null_chk
        CHECK (refs IS NULL OR array_position(refs, NULL) IS NULL);

-- Origins and references have different target vocabularies. Keep the
-- existence checks set-based and lock hot targets before checking them: a
-- concurrent erase must either wait for this admission or win before it, not
-- disappear a target between the check and insertion of the source row.
CREATE OR REPLACE FUNCTION proxima_core.memory_pin_checks() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    pin uuid;
    pin_handle uuid;
BEGIN
    IF NEW.kind = 'fact' AND NEW.origins = '{}' AND NEW.refs = '{}' THEN
        RETURN NEW;
    END IF;

    -- Every hot Memory target is locked in one global t order. Locking origins
    -- and refs in separate phases permits two crossed declarations to take M1
    -- then M2 and M2 then M1, producing a deadlock before either can admit.
    IF NEW.origins <> '{}' OR NEW.refs <> '{}' THEN
        PERFORM 1
          FROM proxima_core.memory
         WHERE t = ANY (NEW.origins || NEW.refs)
         ORDER BY t
         FOR SHARE;
    END IF;
    IF NEW.refs <> '{}' THEN
        PERFORM 1
          FROM proxima_core.goal
         WHERE t = ANY (NEW.refs)
         ORDER BY t
         FOR SHARE;
    END IF;

    IF NEW.kind <> 'fact'
       AND NOT proxima_core.pins_have_grounding_support(
             NEW.origins || NEW.refs, NULL, NULL
           )
    THEN
        RAISE EXCEPTION 'non-fact must pin a hot memory or a cooled fact'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.origins = '{}' AND NEW.refs = '{}' THEN
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
      LEFT JOIN proxima_core.goal g ON g.t = p.id
     WHERE m.t IS NULL AND c.t IS NULL AND g.t IS NULL
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'reference pin % does not exist as a Memory or Goal', pin
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
