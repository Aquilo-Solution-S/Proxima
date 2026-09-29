-- Coarse pin-lock mode for bulk Memory writes.
--
-- memory_pin_checks takes one pg_advisory_xact_lock per distinct target, the
-- row's own t included, and holds it to commit. The shared lock table has
-- max_locks_per_transaction * (max_connections + max_prepared_transactions)
-- slots, so one transaction of a few tens of thousands of rows exhausts it.
--
-- The target locks only exclude a concurrent writer: an erase, forget or
-- colliding admission. Each of those writes memory, cooled or goal, and so
-- holds ROW EXCLUSIVE on it before it changes a target. A transaction that
-- holds SHARE ROW EXCLUSIVE or stronger on all three tables excludes them
-- with three lock entries:
--
--   BEGIN;
--   LOCK TABLE proxima_core.cooled, proxima_core.memory, proxima_core.goal
--       IN SHARE ROW EXCLUSIVE MODE;
--   INSERT INTO proxima_core.memory ...;
--   COMMIT;
--
-- LOCK takes the tables one at a time, in the order forget, erase and goal
-- writes take them; a writer taking memory before cooled (hydration, owner
-- erase) can deadlock with it, and Postgres aborts one of the two.
--
-- After its first 256 target locks, a transaction reads pg_locks once and
-- binds proxima_core.pin_lock_mode transaction-locally: 'coarse' stops taking
-- target locks, 'fine' keeps taking them. Every per-row check is unchanged.
--
-- 0014's body plus the mode, with 0019's search_path. CREATE OR REPLACE keeps
-- the owner and ACL.

CREATE OR REPLACE FUNCTION proxima_core.memory_pin_checks()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, proxima_core, pg_temp
AS $$
DECLARE
    previous_scope text := current_setting('app.proxima_scope', true);
    pin uuid;
    pin_handle uuid;
    historical_restore boolean;
    pin_targets uuid[] := ARRAY[NEW.t] || NEW.origins || NEW.refs || NEW.goal_refs;
    pin_lock_mode text := current_setting('proxima_core.pin_lock_mode', true);
    pin_lock_count integer;
BEGIN
    PERFORM set_config('app.proxima_scope', 'platform', true);
    -- Until decided, the setting counts the target locks this transaction
    -- asked for here. Past 256, one pg_locks read decides the rest of the
    -- transaction; it scans the whole lock table, so a small write never pays
    -- it. 'coarse': memory, cooled and goal are all held in SHARE ROW
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
            SELECT CASE WHEN count(DISTINCT l.relation) = 3 THEN 'coarse' ELSE 'fine' END
              INTO pin_lock_mode
              FROM pg_catalog.pg_locks l
             WHERE l.locktype = 'relation'
               AND l.pid = pg_catalog.pg_backend_pid()
               AND l.granted
               AND l.mode IN ('ShareRowExclusiveLock', 'ExclusiveLock', 'AccessExclusiveLock')
               AND l.relation IN ('proxima_core.memory'::regclass,
                                  'proxima_core.cooled'::regclass,
                                  'proxima_core.goal'::regclass);
        END IF;
        PERFORM set_config('proxima_core.pin_lock_mode', pin_lock_mode, true);
    END IF;
    -- Take the table locks again instead of trusting the setting: held, this
    -- is a local no-op; a setting bound without them acquires them before any
    -- check below reads.
    IF pin_lock_mode = 'coarse' THEN
        LOCK TABLE proxima_core.cooled, proxima_core.memory, proxima_core.goal
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
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
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
        PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
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
    PERFORM set_config('app.proxima_scope', COALESCE(previous_scope, ''), true);
    RETURN NEW;
END;
$$;
