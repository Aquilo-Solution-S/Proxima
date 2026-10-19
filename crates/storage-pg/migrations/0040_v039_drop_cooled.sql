-- #488: forget keeps the Memory row, so nothing writes or reads the cooled
-- table any more. Memories forgotten before this release are not migrated
-- (0038 refused a used store); their cold/<t> purge debts go with the
-- table. The operator deletes the bucket's cold/ prefix by hand.
SELECT set_config('app.proxima_scope', 'platform', true);

DELETE FROM proxima_core.cold_purge_pending
 WHERE object_key LIKE 'cold/%' OR backend = '';

-- The content and memory_external_origin owner policies still reach a
-- cooled parent until the definitions are re-applied after this file, in
-- the same transaction. Drop them by name so the table drops without
-- CASCADE: any other dependent is an error, not a silent loss.
DROP POLICY IF EXISTS proxima_owner_read ON proxima_core.content;
DROP POLICY IF EXISTS proxima_owner_write ON proxima_core.content;
DROP POLICY IF EXISTS proxima_owner_read ON proxima_core.memory_external_origin;
DROP POLICY IF EXISTS proxima_owner_write ON proxima_core.memory_external_origin;

DROP TABLE proxima_core.cooled;

DROP FUNCTION IF EXISTS proxima_core.cooled_append_only();
DROP FUNCTION IF EXISTS proxima_core.cooled_erase_witness();
DROP FUNCTION IF EXISTS proxima_core.cooled_identity_seal();

-- memory_pin_checks loses its cooled restoration seal with the table.
-- 0035 installed the seal in a migration, so the seal leaves in one: this
-- is the body the definitions refresh installs. The other functions that
-- named cooled are replaced by that refresh, in this same transaction.
CREATE OR REPLACE FUNCTION proxima_core.memory_pin_checks()
 RETURNS trigger
 LANGUAGE plpgsql
 SECURITY DEFINER
 SET search_path TO 'pg_catalog', 'proxima_core', 'pg_temp'
AS $function$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
    pin uuid;
    pin_handle uuid;
    pin_targets uuid[] := ARRAY[NEW.t] || NEW.origins || NEW.refs || NEW.goal_refs;
    pin_lock_mode text := current_setting('proxima_core.pin_lock_mode', true);
    pin_lock_count integer;
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    -- Until decided, the setting counts the target locks this transaction
    -- asked for here. Past 256, one pg_locks read decides the rest of the
    -- transaction; it scans the whole lock table, so a small write never pays
    -- it. 'coarse': memory and goal are both held in SHARE ROW
    -- EXCLUSIVE or stronger, which excludes every writer a target lock waits
    -- for. Anything else is 'fine'.
    IF pin_lock_mode IS DISTINCT FROM 'coarse'
       AND pin_lock_mode IS DISTINCT FROM 'fine'
    THEN
        pin_lock_count := COALESCE(
            substring(pin_lock_mode FROM '^[0-9]{1,6}$')::integer, 0
        ) + cardinality(pin_targets);
        IF pin_lock_count <= 256 THEN
            pin_lock_mode := pin_lock_count::text;
        ELSE
            SELECT CASE WHEN count(DISTINCT l.relation) = 2 THEN 'coarse' ELSE 'fine' END
              INTO pin_lock_mode
              FROM pg_catalog.pg_locks l
             WHERE l.locktype = 'relation'
               AND l.pid = pg_catalog.pg_backend_pid()
               AND l.granted
               AND l.mode IN ('ShareRowExclusiveLock', 'ExclusiveLock', 'AccessExclusiveLock')
               AND l.relation IN ('proxima_core.memory'::regclass,
                                  'proxima_core.goal'::regclass);
        END IF;
        PERFORM set_config('proxima_core.pin_lock_mode', pin_lock_mode, true);
    END IF;
    -- Take the table locks again instead of trusting the setting: held, this
    -- is a local no-op; a setting bound without them acquires them before any
    -- check below reads.
    IF pin_lock_mode = 'coarse' THEN
        LOCK TABLE proxima_core.memory, proxima_core.goal
            IN SHARE ROW EXCLUSIVE MODE;
    ELSE
        PERFORM proxima_core.lock_pin_targets(pin_targets);
    END IF;

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

    IF NEW.kind = 'fact'
       AND NEW.origins = '{}'
       AND NEW.refs = '{}'
       AND NEW.goal_refs = '{}'
    THEN
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;

    -- Goal references never provide F/A/P grounding.
    IF NEW.kind <> 'fact'
       AND NOT proxima_core.pins_have_grounding_support(
             NEW.origins || NEW.refs, NULL, NULL
           )
    THEN
        RAISE EXCEPTION 'non-fact must pin a memory'
            USING ERRCODE = '23514';
    END IF;

    IF NEW.origins = '{}' AND NEW.refs = '{}' AND NEW.goal_refs = '{}' THEN
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
        RETURN NEW;
    END IF;

    -- Origins are always Memory targets; a witness never satisfies them.
    SELECT p.id INTO pin
      FROM unnest(NEW.origins) AS p(id)
      LEFT JOIN proxima_core.memory m ON m.t = p.id
     WHERE m.t IS NULL
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'origin pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    -- `refs` carries only Memory targets after the 0004 split.
    SELECT p.id INTO pin
      FROM unnest(NEW.refs) AS p(id)
      LEFT JOIN proxima_core.memory m ON m.t = p.id
     WHERE m.t IS NULL
     LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'reference pin % does not exist as a Memory', pin
            USING ERRCODE = '23503';
    END IF;

    -- `goal_refs` carries only Goal targets.
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
        ) THEN
            RAISE EXCEPTION 'perspective origins must be abstraction t'
                USING ERRCODE = '23514';
        END IF;
    END IF;
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$function$;
REVOKE ALL ON FUNCTION proxima_core.memory_pin_checks() FROM PUBLIC;
