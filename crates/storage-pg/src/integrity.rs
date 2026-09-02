//! Declaration integrity: the floor under every write path, and the check
//! that reads it back.
//!
//! One invariant, stated three ways, because the array foreign key
//! `PostgreSQL` has no syntax for takes three statements to say (and four
//! trigger families to enforce, since the third is guarded at both ends):
//!
//! 1. **stamp ⊆ registry** — a stamp names only tables some flavor
//!    declares. `assert_sidecar_stamp_declared` (`0001_v008.sql`).
//! 2. **row ⊆ stamp** — a row in a registered memory-sidecar table exists
//!    only for a memory row whose `sidecar_tables` declares that table.
//!    [`DECLARATION_TRIGGER_FUNCTION`], v0.0.9.
//! 3. **stamp ⊆ rows** — a memory row that declares a table has a row in
//!    it. [`PRESENCE_TRIGGER_FUNCTION`], `0009_declared_sidecar_presence.sql`,
//!    and the reason this module is not two halves any more.
//!
//! Direction 3 is not symmetry for its own sake. A stamp with no row cools
//! into a cold object whose sidecar dump can never equal its own stamp, so
//! the Memory forgets and then cannot be hydrated, permanently, with no
//! error at the moment the damage is done —
//! `PgSidecarRegistryFrozen::integrity_check` could not even find it, since
//! it only ever asked direction 2. Forget now refuses such a row at cool
//! time (`dump_stamped_sidecars`); this refuses it at the transaction that
//! creates it, which is the only place the operator still has the
//! information to fix it.
//!
//! Direction 3 is guarded at both ends. A stamp can lose its row by never
//! having written one ([`PRESENCE_TRIGGER_FUNCTION`], at `INSERT`) or by
//! having its row deleted out from under it ([`DELETE_GUARD_FUNCTION`], at
//! `DELETE`). The second is strictly worse than the first: the row's bytes
//! are gone, so nothing can repair it — `integrity_check` can name the damage
//! and no operator can undo it. That asymmetry is why the `DELETE` is refused
//! rather than reported.
//!
//! **What it costs, measured on PG 18.4.** The presence trigger: 8.8 µs of
//! `COMMIT` per stamped surface, 0.01 µs when the memory does not stamp it —
//! the `WHEN` clause is evaluated on the queueing path. The orphan guard:
//! 3.0 µs of `COMMIT` per deleted sidecar row, taking a 300k-row delete from
//! 1.21 s to 2.14 s, which lands on owner erase and on forget. A constraint
//! trigger must be `FOR EACH ROW` and cannot take a transition table
//! (`REFERENCING OLD TABLE` on one is a syntax error), so the set-based
//! statement-level formulation that would have cost one join per statement
//! does not exist; per-row is the only shape there is. A second on a 300k-row
//! compliance batch is the right price for a corruption class with no repair.
//!
//! **What is still not enforced: rows already there, and writes that turn
//! the triggers off.** These triggers constrain writes from the moment they
//! are installed. A database upgraded in place may already carry a stamp
//! with no row; so may one an owner reached with `TRUNCATE`, which fires no
//! row trigger at all, or with `session_replication_role = replica`. Nothing
//! here finds any of them — [`IntegrityFinding::MissingStampedSidecarRows`]
//! is the read-back that does, and running it once after an upgrade or a
//! restore is the operator's job (`docs/how-to/operate.md`).
//!
//! Why the database and not the port. Forget, owner erase and owner export
//! all walk `memory.sidecar_tables`; a row nobody stamped is reachable by
//! none of them, so it survives a forget, escapes an erase and is missing
//! from an export, silently and forever. A check in the write path guards
//! the write path. This guards hand-written SQL, a psql session, an
//! operator's backfill and every path not yet written — which is what
//! "below every path" has to mean if the word is doing any work.
//!
//! Two halves, mirroring [`crate::projection`] exactly:
//!
//! 1. **The generator**, four families of it. [`declaration_trigger`]
//!    emits direction 2's `BEFORE INSERT` guard,
//!    [`key_repoint_trigger`] emits the `BEFORE UPDATE OF <key>` guard that
//!    closes the same direction against a re-pointed key column,
//!    [`presence_trigger`] emits direction 3's deferred constraint trigger on
//!    `proxima_core.memory`, and [`delete_guard_trigger`] emits direction 3's
//!    other end — the deferred `AFTER DELETE` guard on the sidecar table
//!    itself. Each carries its own `DROP`. All of it is pasted
//!    verbatim into migrations — the declaration lane
//!    (`0002_v009_declaration_triggers.sql` and each flavor's own) for the
//!    first family, the presence lane
//!    (`0009_declared_sidecar_presence.sql` and each flavor's own) for the
//!    other three — and pinned by
//!    `generated_declaration_triggers_are_the_migration_text` and
//!    `generated_presence_triggers_are_the_migration_text`, so the migration
//!    author cannot drift from the generator. Deliberately NOT into the
//!    v0.0.8 baselines, and the declaration lane is not edited either: both
//!    are applied, and editing one would change the checksum of a version live
//!    databases have already applied, turning an additive release into a
//!    forced reset (docs/how-to/migrations.md).
//! 2. **The boot guardrail** ([`ensure_declaration_triggers`]), which re-runs
//!    all four generators against the frozen registry and compares the whole
//!    definition `pg_get_triggerdef` renders back for each. The whole
//!    definition and not a fragment: a `WHEN` clause that never matches
//!    disarms a presence trigger completely, and neither that nor a lost
//!    `DEFERRABLE` shows up in the argument list. It issues no DDL: in the split-role topology (docs/15) an
//!    init container or `tools/dev-migrate` migrates under a DDL role and the
//!    app boots under a DML-only role that could not create a trigger if it
//!    tried. A flavor whose migrations have not been applied therefore fails
//!    at boot, the same way and in the same place a missing projection table
//!    does.
//!
//! [`PgSidecarRegistryFrozen::integrity_check`] is the read-back: the same
//! invariant asked of rows that are already there, beside
//! [`PgSidecarRegistryFrozen::rebuild_projection_for_table`].
//!
//! [`PgSidecarRegistryFrozen::integrity_check`]: crate::sidecars::PgSidecarRegistryFrozen::integrity_check
//! [`PgSidecarRegistryFrozen::rebuild_projection_for_table`]: crate::sidecars::PgSidecarRegistryFrozen::rebuild_projection_for_table

use proxima_core::StorageError;

use crate::pg_ident::PgIdent;
use crate::projection::Artifact;
use crate::sidecars::PgSidecarRegistryFrozen;

/// The qualified name of the one function every declaration trigger runs.
pub const DECLARATION_TRIGGER_FUNCTION_NAME: &str = "proxima_core.assert_memory_declares_sidecar";

/// What a generated trigger is called: the relation's own name, suffixed.
const DECLARATION_TRIGGER_SUFFIX: &str = "declared_by_memory";

/// ONE plpgsql function for every guarded table, not one per table.
///
/// The two facts a per-table function would hardcode are both available to
/// a shared one: the surface is `TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME`,
/// which is the table the trigger is installed on and therefore cannot
/// drift from it, and the memory-key column arrives as `TG_ARGV[0]` from
/// the generated `CREATE TRIGGER`. Reading a column named at runtime costs
/// one `to_jsonb(NEW)`.
///
/// **Measured, PG 18.4, 6 rotated rounds of 20k single-row inserts,
/// minimum per arm:** untriggered 8.63 µs/insert, per-table function
/// (static `NEW.<col>`) 12.86, this shared function 14.58. So the guard
/// costs 4.2 µs and reading the column dynamically costs a further 1.8.
/// The 1.8 buys ONE function body in the tree instead of one per
/// registered sidecar table — twenty-six of them today, and one more for
/// every sidecar any flavor ever adds — and it buys that the logic has a
/// single place to be read and a single place to be wrong. Against a real
/// fact ingest (admission, receipt, ledger, embedding enqueue) neither
/// number is visible; against the DDL it is the difference between a
/// baseline that states the rule once and a baseline that restates it
/// twenty-six times.
///
/// `ERRCODE 23503` is `foreign_key_violation`, the same code
/// `assert_sidecar_stamp_declared` raises. They are two directions of one
/// array foreign key, so they report as one kind of thing.
pub const DECLARATION_TRIGGER_FUNCTION: &str = r"CREATE OR REPLACE FUNCTION proxima_core.assert_memory_declares_sidecar() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME;
    memory_t uuid := (to_jsonb(NEW) ->> TG_ARGV[0])::uuid;
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.t = memory_t
           AND m.sidecar_tables @> ARRAY[surface]
    ) THEN
        RAISE EXCEPTION
            'sidecar row in % for memory %, which does not declare % in memory.sidecar_tables',
            surface, memory_t, surface
            USING ERRCODE = '23503',
                  HINT = 'forget, owner erase and owner export all reach a sidecar row '
                         || 'through memory.sidecar_tables and nowhere else, so an undeclared '
                         || 'row is reachable by none of them; write through '
                         || 'Engine::unit_of_work, which stamps the memory row with every '
                         || 'table the write touches, in the same transaction and before the '
                         || 'sidecar row';
    END IF;
    RETURN NEW;
END;
$$;";

/// The relation half of a schema-qualified sidecar table.
///
/// Every generated trigger name is derived from it, and the boot guardrail
/// derives the same names from the same function — a trigger name formatted
/// twice is a trigger name that can drift once.
fn relation_of(sidecar_table: &str) -> Result<(&str, &str), StorageError> {
    sidecar_table.split_once('.').ok_or_else(|| {
        StorageError::Internal(format!(
            "a registered sidecar table must be schema-qualified: {sidecar_table:?}"
        ))
    })
}

/// The name of the `BEFORE INSERT` declaration trigger on `sidecar_table`.
///
/// # Errors
///
/// [`StorageError::Internal`] when the table is not schema-qualified, or when
/// the derived name exceeds `PostgreSQL`'s 63-byte identifier limit.
fn declaration_trigger_name(sidecar_table: &str) -> Result<String, StorageError> {
    let (_, relation) = relation_of(sidecar_table)?;
    Ok(
        PgIdent::column(&format!("{relation}_{DECLARATION_TRIGGER_SUFFIX}"))?
            .as_str()
            .to_owned(),
    )
}

/// The name of the `BEFORE UPDATE OF <key>` declaration trigger on
/// `sidecar_table`.
///
/// # Errors
///
/// As [`declaration_trigger_name`].
fn key_repoint_trigger_name(sidecar_table: &str) -> Result<String, StorageError> {
    let (_, relation) = relation_of(sidecar_table)?;
    Ok(
        PgIdent::column(&format!("{relation}_{KEY_REPOINT_TRIGGER_SUFFIX}"))?
            .as_str()
            .to_owned(),
    )
}

/// The name of the presence constraint trigger guarding `sidecar_table`.
/// It lives on `proxima_core.memory`, so the name carries the schema.
///
/// # Errors
///
/// As [`declaration_trigger_name`].
fn presence_trigger_name(sidecar_table: &str) -> Result<String, StorageError> {
    let (schema, relation) = relation_of(sidecar_table)?;
    Ok(
        PgIdent::column(&format!("{PRESENCE_TRIGGER_PREFIX}_{schema}_{relation}"))?
            .as_str()
            .to_owned(),
    )
}

/// The name of the `AFTER DELETE` orphan guard on `sidecar_table`.
///
/// # Errors
///
/// As [`declaration_trigger_name`].
fn delete_guard_trigger_name(sidecar_table: &str) -> Result<String, StorageError> {
    let (_, relation) = relation_of(sidecar_table)?;
    Ok(
        PgIdent::column(&format!("{relation}_{DELETE_GUARD_TRIGGER_SUFFIX}"))?
            .as_str()
            .to_owned(),
    )
}

/// The declaration trigger for one registered memory-sidecar table, and the
/// statement that removes it.
///
/// `CREATE OR REPLACE TRIGGER` rather than `DROP … IF EXISTS` + `CREATE`:
/// the baselines already require `uuidv7()`
/// (`proxima_core.memory.t DEFAULT uuidv7()`), which is `PostgreSQL` 18, and
/// replace-a-trigger has been available since 14. One statement per table
/// instead of two is half the generated DDL for the same idempotence.
///
/// `memory_key_column` is the column the table stores its memory `t` under,
/// as its own registration declares it — never a literal `t`. A sidecar may
/// key on any column (`KeyShape::MemoryT { column }`), and a guard that
/// assumed `t` would read NULL on such a table and refuse every insert into
/// it.
///
/// # Errors
///
/// [`StorageError::Internal`] when the table is not a schema-qualified
/// identifier, when the key column is not an identifier, or when the
/// trigger name derived from the relation exceeds `PostgreSQL`'s 63-byte
/// identifier limit — a silent truncation there would give two tables one
/// trigger name and make the boot guardrail unsatisfiable.
pub fn declaration_trigger(
    sidecar_table: &str,
    memory_key_column: &str,
) -> Result<Artifact, StorageError> {
    let table = PgIdent::table(sidecar_table)?;
    let trigger = declaration_trigger_name(sidecar_table)?;
    let key = PgIdent::column(memory_key_column)?;

    // SQL-POLICY: PgIdent
    // The relation, the derived trigger name and the key column are all
    // validated identifiers; the key column is spliced as a trigger
    // argument literal, and `PgIdent` admits no quote to close it with.
    // Everything else is fixed text, identical for every table.
    let forward = format!(
        "CREATE OR REPLACE TRIGGER {trigger}
    BEFORE INSERT ON {table}
    FOR EACH ROW
    EXECUTE FUNCTION {DECLARATION_TRIGGER_FUNCTION_NAME}('{key}');",
        table = table.as_str(),
        key = key.as_str()
    );
    // SQL-POLICY: PgIdent
    let inverse = format!("DROP TRIGGER IF EXISTS {trigger} ON {};", table.as_str());
    Ok(Artifact { forward, inverse })
}

/// The one relation every presence trigger is installed on.
const MEMORY_TABLE: &str = "proxima_core.memory";

/// The qualified name of the one function every presence trigger runs.
pub const PRESENCE_TRIGGER_FUNCTION_NAME: &str = "proxima_core.assert_declared_sidecar_present";

/// What a generated presence trigger is called: the guarded surface, with
/// its dot flattened, under one prefix. Every one of them lives on
/// `proxima_core.memory`, so the relation cannot disambiguate them and the
/// name has to carry the schema — `proxima_core.chunk_v1` and
/// `proxima_code.chunk_v1` are two tables and must be two triggers.
const PRESENCE_TRIGGER_PREFIX: &str = "memory_declares";

/// What the `UPDATE` half of the declaration guard is called.
const KEY_REPOINT_TRIGGER_SUFFIX: &str = "declared_by_memory_on_update";

/// What a generated orphan guard is called. Same family, same reading: the
/// row is declared by a memory, and the `DELETE` that would end that has to
/// end the declaration with it.
const DELETE_GUARD_TRIGGER_SUFFIX: &str = "declared_by_memory_on_delete";

/// The qualified name of the one function every orphan guard runs.
pub const DELETE_GUARD_FUNCTION_NAME: &str = "proxima_core.assert_row_not_still_declared";

/// Direction 3, at the other end: a stamped sidecar row may not be deleted
/// out from under its stamp.
///
/// This is the same corruption [`PRESENCE_TRIGGER_FUNCTION`] refuses at
/// `INSERT`, arrived at by `DELETE` instead. A stamp whose row is gone cools
/// into a cold object whose dump can never equal its own stamp, so the Memory
/// forgets and can never be hydrated — and unlike the insert case there is no
/// repair, because the row's bytes are gone. `integrity_check` can name the
/// damage; nothing can undo it. That asymmetry is the argument for refusing
/// the `DELETE` rather than reporting it.
///
/// `DEFERRABLE INITIALLY DEFERRED`, and it has to be: the sidecar's own
/// foreign key to `proxima_core.memory` has no `ON DELETE CASCADE`
/// (`0001_v008.sql`), so a legitimate delete of both rows must take the
/// sidecar row FIRST. An immediate check would see the memory row still
/// standing and refuse every forget and every owner erase. Deferred to
/// `COMMIT`, it sees the transaction's outcome: both gone, or neither.
///
/// **Measured, PG 18.4:** deleting 300k sidecar rows and their memory rows in
/// one transaction costs 1.21 s without this guard and 2.14 s with —
/// 3.0 µs of `COMMIT` per deleted row. It lands on owner erase, the one lane
/// that deletes sidecar rows in bulk, and on forget, which deletes a handful
/// per memory. A constraint trigger must be `FOR EACH ROW` and cannot take a
/// transition table (`REFERENCING OLD TABLE` is a syntax error on one), so
/// the set-based statement-level formulation that would have cost one join
/// per statement is not available. Per-row is the only shape there is, and
/// a second on a 300k-row compliance batch is the right price for a
/// corruption class with no repair.
///
/// Owner-pinned sidecars get no orphan guard, for the same reason they get no
/// presence trigger: a source-scoped erase legitimately takes their row while
/// a Memory that has since transferred still stamps it.
///
/// The key column is read as `to_jsonb(OLD) ->> TG_ARGV[1]` rather than
/// `OLD.t`, so one function serves a table keyed on anything — the same move
/// [`DECLARATION_TRIGGER_FUNCTION`] makes on `NEW`.
pub const DELETE_GUARD_FUNCTION: &str = r"CREATE OR REPLACE FUNCTION proxima_core.assert_row_not_still_declared() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_ARGV[0];
    memory_t uuid := (to_jsonb(OLD) ->> TG_ARGV[1])::uuid;
BEGIN
    IF EXISTS (
        SELECT 1
          FROM proxima_core.memory m
         WHERE m.t = memory_t
           AND m.sidecar_tables @> ARRAY[surface]
    ) THEN
        RAISE EXCEPTION
            'memory % still declares % in memory.sidecar_tables, so the row it declares there may not be deleted',
            memory_t, surface
            USING ERRCODE = '23503',
                  HINT = 'a stamp whose row was deleted cools into a cold object whose '
                         || 'sidecar dump cannot equal its own stamp, so the Memory forgets '
                         || 'and can never be hydrated, and the row is gone so nothing can '
                         || 'repair it; delete the memory row in the same transaction, which '
                         || 'is what forget and owner erase do';
    END IF;
    RETURN NULL;
END;
$$;";

/// Direction 3 against a `DELETE` of the stamped row, and the statement that
/// removes it.
///
/// A `CONSTRAINT TRIGGER`, deferred, for the reason spelled out on
/// [`DELETE_GUARD_FUNCTION`]. No `WHEN` clause: unlike the presence trigger,
/// this one lives on the guarded table itself, so every row it sees is a row
/// it is responsible for.
///
/// `DROP` + `CREATE`, like [`presence_trigger`]: `CREATE OR REPLACE
/// CONSTRAINT TRIGGER` is not supported.
///
/// # Errors
///
/// As [`presence_trigger`].
pub fn delete_guard_trigger(
    sidecar_table: &str,
    memory_key_column: &str,
) -> Result<Artifact, StorageError> {
    let table = PgIdent::table(sidecar_table)?;
    let trigger = delete_guard_trigger_name(sidecar_table)?;
    let key = PgIdent::column(memory_key_column)?;

    // SQL-POLICY: PgIdent
    // Relation, derived trigger name and key column are all validated
    // identifiers; the surface and the key are spliced into the argument
    // list as literals, and `PgIdent` admits no quote to close either with.
    let forward = format!(
        "DROP TRIGGER IF EXISTS {trigger} ON {table};
CREATE CONSTRAINT TRIGGER {trigger}
    AFTER DELETE ON {table}
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    EXECUTE FUNCTION {DELETE_GUARD_FUNCTION_NAME}('{table}', '{key}');",
        table = table.as_str(),
        key = key.as_str()
    );
    // SQL-POLICY: PgIdent
    let inverse = format!("DROP TRIGGER IF EXISTS {trigger} ON {};", table.as_str());
    Ok(Artifact { forward, inverse })
}

/// Direction 3's shared function: the stamp names a table, so the table has
/// a row.
///
/// Both facts it needs arrive as trigger arguments rather than from the
/// catalog: the guarded surface as `TG_ARGV[0]` and its memory-key column as
/// `TG_ARGV[1]`. The surface cannot be read off the trigger's own relation
/// the way [`DECLARATION_TRIGGER_FUNCTION`] reads it, because every one of
/// these triggers is installed on `proxima_core.memory` and the surface is
/// the thing that varies.
///
/// **Measured, PG 18.4, 200k memory rows inserted and stamped in one
/// transaction, `COMMIT` time only:** 0.01 µs/row when the stamp does not
/// name the guarded table, 8.8 µs/row when it does. The first number is the
/// `WHEN` clause the generator emits: `PostgreSQL` evaluates it before
/// queueing the deferred event, so a memory row that stamps one table pays
/// for one table and not for the twenty-six the deployment registers. The
/// second is one `EXECUTE` — a shared function cannot name the table
/// statically, and a one-shot plan for `SELECT EXISTS` on a primary key is
/// where 6 of those 8.8 µs go. Against a real fact ingest neither is
/// visible, and both land on `COMMIT`, behind an fsync.
///
/// `RETURN NULL` because an `AFTER` trigger's return value is discarded.
///
/// `ERRCODE 23503` is `foreign_key_violation`, the code the other two
/// directions raise. Three directions of one array foreign key report as one
/// kind of thing.
///
/// The body re-tests membership that the `WHEN` clause has already tested.
/// That is deliberate and it is not redundant: `WHEN` is an optimisation —
/// it keeps an unstamped table off the queueing path — and an optimisation
/// is the wrong place for the only copy of a rule. A trigger carrying this
/// function under the right name and arguments but a `WHEN` that never
/// matches would admit exactly the state this direction exists to refuse,
/// and a trigger with no `WHEN` at all would refuse every write that does
/// not stamp the table. With the test in the body, both are merely slow or
/// merely safe rather than wrong. The array scan is over a stamp with a
/// handful of elements and does not appear against the `EXECUTE` below.
pub const PRESENCE_TRIGGER_FUNCTION: &str = r"CREATE OR REPLACE FUNCTION proxima_core.assert_declared_sidecar_present() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    surface text := TG_ARGV[0];
    present boolean;
BEGIN
    IF NOT (surface = ANY (NEW.sidecar_tables)) THEN
        RETURN NULL;
    END IF;
    EXECUTE format('SELECT EXISTS (SELECT 1 FROM %s WHERE %I = $1)', surface, TG_ARGV[1])
       INTO present
      USING NEW.t;
    IF NOT present THEN
        RAISE EXCEPTION
            'memory % declares % in memory.sidecar_tables with no row in that table',
            NEW.t, surface
            USING ERRCODE = '23503',
                  HINT = 'a stamp with no row cools into a cold object whose sidecar dump '
                         || 'cannot equal its own stamp, so the Memory forgets and can never '
                         || 'be hydrated; write through Engine::unit_of_work, which stamps '
                         || 'exactly the tables the write inserts into, in the same '
                         || 'transaction';
    END IF;
    RETURN NULL;
END;
$$;";

/// Direction 3 for one guarded sidecar table, and the statement that removes
/// it.
///
/// A `CONSTRAINT TRIGGER`, `DEFERRABLE INITIALLY DEFERRED`, because the
/// memory row is inserted before the sidecar rows it stamps — the sidecar's
/// own foreign key to `proxima_core.memory` requires exactly that order — so
/// an immediate check would refuse every legitimate write. Deferred to
/// `COMMIT`, the two rows are both there or the transaction is not.
///
/// `DROP` + `CREATE` rather than the `CREATE OR REPLACE TRIGGER` the other
/// two families use: `PostgreSQL` 18 answers `CREATE OR REPLACE CONSTRAINT
/// TRIGGER` with `is not supported`. The `DROP … IF EXISTS` in front buys
/// back the same idempotent replay.
///
/// `AFTER INSERT OR UPDATE OF sidecar_tables` mirrors
/// `memory_sidecar_stamp_declared` in the v0.0.8 baseline. The `UPDATE` arm
/// is unreachable today — `memory_owner_or_append_only` raises `25006` on any
/// `UPDATE` that moves `sidecar_tables` — and is spelled out anyway so that a
/// future migration which unfreezes the column does not silently unfreeze the
/// invariant with it.
///
/// The `WHEN` clause is not an optimisation of the function body: it is
/// evaluated on the queueing path, so an unstamped table costs nothing at all
/// rather than one queued event and one function call. See
/// [`PRESENCE_TRIGGER_FUNCTION`] for the two numbers.
///
/// # Errors
///
/// [`StorageError::Internal`] when the table is not a schema-qualified
/// identifier, when the key column is not an identifier, or when the derived
/// trigger name exceeds `PostgreSQL`'s 63-byte identifier limit — silent
/// truncation there would give two guarded surfaces one trigger name on the
/// one relation they share, and make the boot guardrail unsatisfiable.
pub fn presence_trigger(
    sidecar_table: &str,
    memory_key_column: &str,
) -> Result<Artifact, StorageError> {
    let table = PgIdent::table(sidecar_table)?;
    let trigger = presence_trigger_name(sidecar_table)?;
    let key = PgIdent::column(memory_key_column)?;

    // SQL-POLICY: PgIdent
    // The surface, the derived trigger name and the key column are all
    // validated identifiers; the surface and the key are spliced as literals
    // — into the `WHEN` comparison and the trigger argument list — and
    // `PgIdent` admits no quote to close either with. Everything else is
    // fixed text, identical for every guarded table.
    let forward = format!(
        "DROP TRIGGER IF EXISTS {trigger} ON {MEMORY_TABLE};
CREATE CONSTRAINT TRIGGER {trigger}
    AFTER INSERT OR UPDATE OF sidecar_tables ON {MEMORY_TABLE}
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ('{table}' = ANY (NEW.sidecar_tables))
    EXECUTE FUNCTION {PRESENCE_TRIGGER_FUNCTION_NAME}('{table}', '{key}');",
        table = table.as_str(),
        key = key.as_str()
    );
    // SQL-POLICY: PgIdent
    let inverse = format!("DROP TRIGGER IF EXISTS {trigger} ON {MEMORY_TABLE};");
    Ok(Artifact { forward, inverse })
}

/// Direction 2 against an `UPDATE` of the key column, and the statement that
/// removes it.
///
/// [`declaration_trigger`] is `BEFORE INSERT` only, which leaves one door
/// open: re-point an existing sidecar row's memory-key column at a memory
/// that does not stamp the table, and nothing fires. The row's foreign key
/// is satisfied — the new memory exists — and the row is now invisible to
/// forget, owner erase and owner export. Four of the six core sidecars are
/// closed to this by an append-only `UPDATE` trigger of their own;
/// `write_act_v1` and `mcp_call_logged_v1` are not, and no flavor sidecar is
/// obliged to be.
///
/// It runs [`DECLARATION_TRIGGER_FUNCTION`] unchanged: that function reads
/// `NEW`, which an `UPDATE` trigger has, and asks the one question this arm
/// also asks. A second body would be a second place for one rule to be wrong.
///
/// `UPDATE OF {key}` and not a bare `UPDATE`: an `UPDATE` that leaves the key
/// alone cannot break the direction, and paying for it would tax every
/// legitimate column rewrite on every sidecar table.
///
/// The key column may not be a reserved word. `UPDATE OF` is the one place
/// the generators spell an identifier where `PostgreSQL`'s grammar will not
/// take one unquoted, so a sidecar keyed on `user` produces `syntax error at
/// or near "user"` — at the first apply of the migration that carries this
/// artifact, before any row exists, which is why the check is this paragraph
/// and not a keyword table. `PgIdent` cannot catch it: the name is a valid
/// identifier, just not a bare one.
///
/// # Errors
///
/// As [`declaration_trigger`].
pub fn key_repoint_trigger(
    sidecar_table: &str,
    memory_key_column: &str,
) -> Result<Artifact, StorageError> {
    let table = PgIdent::table(sidecar_table)?;
    let trigger = key_repoint_trigger_name(sidecar_table)?;
    let key = PgIdent::column(memory_key_column)?;

    // SQL-POLICY: PgIdent
    let forward = format!(
        "CREATE OR REPLACE TRIGGER {trigger}
    BEFORE UPDATE OF {key} ON {table}
    FOR EACH ROW
    EXECUTE FUNCTION {DECLARATION_TRIGGER_FUNCTION_NAME}('{key}');",
        table = table.as_str(),
        key = key.as_str()
    );
    // SQL-POLICY: PgIdent
    let inverse = format!("DROP TRIGGER IF EXISTS {trigger} ON {};", table.as_str());
    Ok(Artifact { forward, inverse })
}

/// One trigger the registry says must exist, and the whole definition
/// `pg_get_triggerdef` must render back for it.
///
/// The whole definition, not a fragment. A `WHEN` clause that never matches
/// disarms a presence trigger completely, and a trigger that is not
/// `DEFERRABLE INITIALLY DEFERRED` refuses every legitimate write; neither
/// shows up in the argument list, so neither is visible to a check that only
/// looks for `EXECUTE FUNCTION …(args)`. Equality sees all of it — timing,
/// events, column list, deferral, `WHEN`, and the arguments exactly rather
/// than as a substring.
///
/// This pins `PostgreSQL`'s own rendering, which is deterministic but not
/// contractual. That is on purpose: every PG test in the workspace boots
/// through this function, so a rendering change in a future major version
/// fails in CI, loudly, instead of degrading a production guard into one
/// that passes whatever it finds.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ExpectedTrigger {
    relation: String,
    name: String,
    definition: String,
}

/// The set [`ensure_declaration_triggers`] expects, built from the registry
/// alone, and sorted into the order `pg_trigger` is read back in.
fn expected_declaration_triggers(
    registry: &PgSidecarRegistryFrozen,
) -> Result<Vec<ExpectedTrigger>, StorageError> {
    let mut declared: Vec<ExpectedTrigger> = Vec::new();
    for table in registry.memory_sidecar_tables() {
        let key = registry.memory_key_column(table).ok_or_else(|| {
            StorageError::Internal(format!(
                "{table} is a registered memory sidecar with no declared memory-key column; \
                 give its `pg_sidecar!` registration a `key:` and its contract a \
                 KeyShape::MemoryT {{ column }}"
            ))
        })?;
        let declaration = declaration_trigger_name(table)?;
        declared.push(ExpectedTrigger {
            definition: format!(
                "CREATE TRIGGER {declaration} BEFORE INSERT ON {table} FOR EACH ROW \
                 EXECUTE FUNCTION {DECLARATION_TRIGGER_FUNCTION_NAME}('{key}')"
            ),
            relation: table.to_owned(),
            name: declaration,
        });
        let repoint = key_repoint_trigger_name(table)?;
        declared.push(ExpectedTrigger {
            definition: format!(
                "CREATE TRIGGER {repoint} BEFORE UPDATE OF {key} ON {table} FOR EACH ROW \
                 EXECUTE FUNCTION {DECLARATION_TRIGGER_FUNCTION_NAME}('{key}')"
            ),
            relation: table.to_owned(),
            name: repoint,
        });
        if !registry.is_owner_pinned_memory_sidecar_table(table) {
            let orphan = delete_guard_trigger_name(table)?;
            declared.push(ExpectedTrigger {
                definition: format!(
                    "CREATE CONSTRAINT TRIGGER {orphan} AFTER DELETE ON {table} DEFERRABLE \
                     INITIALLY DEFERRED FOR EACH ROW \
                     EXECUTE FUNCTION {DELETE_GUARD_FUNCTION_NAME}('{table}', '{key}')"
                ),
                relation: table.to_owned(),
                name: orphan,
            });
            let presence = presence_trigger_name(table)?;
            declared.push(ExpectedTrigger {
                definition: format!(
                    "CREATE CONSTRAINT TRIGGER {presence} AFTER INSERT OR UPDATE OF \
                     sidecar_tables ON {MEMORY_TABLE} DEFERRABLE INITIALLY DEFERRED FOR EACH ROW \
                     WHEN (('{table}'::text = ANY (new.sidecar_tables))) \
                     EXECUTE FUNCTION {PRESENCE_TRIGGER_FUNCTION_NAME}('{table}', '{key}')"
                ),
                relation: MEMORY_TABLE.to_owned(),
                name: presence,
            });
        }
    }
    declared.sort();
    Ok(declared)
}

/// Every declaration trigger the linked flavors expect, as `pg_trigger` sees
/// it — and nothing else.
///
/// All four families at once (see the module docs): row ⊆ stamp on `INSERT`
/// and on an `UPDATE` of the key column, both on the sidecar table, and
/// stamp ⊆ rows at both ends — the presence trigger on
/// `proxima_core.memory` and the orphan guard on the sidecar's own `DELETE`.
/// The last two are skipped for an owner-pinned sidecar, and that is not an
/// oversight: an owner-pinned row
/// carries its own `owner_id` and no foreign key to `proxima_core.memory`
/// precisely so a source-scoped erase can take it while the Memory it
/// records — which may since have transferred to another owner — stays. Its
/// stamp is a record of what was written, not a claim about what is still
/// there, and neither a guard that demanded the row still be there nor one
/// that refused to let the erase take it could hold.
///
/// Bidirectional, like [`crate::projection::ensure_projection_schema`]: a
/// registered sidecar table with no trigger is a table any statement can
/// write undeclared rows into, and a trigger on a table no linked flavor
/// registers is a guard nobody maintains. Both are the same drift as a
/// missing migration, so both fail here, at boot, before a write.
///
/// Read-only by construction. See the module docs on the split-role deploy.
///
/// # Errors
///
/// [`StorageError::Internal`] when the trigger set in the database is not
/// the set the registry declares, when a trigger runs on a different
/// memory-key column or guarded surface than its registration declares, or
/// when a registered memory sidecar declares no memory-key column at all.
pub async fn ensure_declaration_triggers(
    pool: &sqlx::PgPool,
    registry: &PgSidecarRegistryFrozen,
) -> Result<(), StorageError> {
    let declared = expected_declaration_triggers(registry)?;

    // `pg_get_triggerdef` renders the argument list back as `SQL`, so the
    // key column and guarded surface a trigger actually runs on are readable
    // without decoding `pg_trigger.tgargs`' NUL-separated bytea by hand.
    //
    // That rendering, though, is not a constant: it is a function of two
    // session settings as well as of the trigger.
    //
    // `search_path` — PG omits the schema of anything the path already
    // resolves, so a session with `proxima_core` on its path gets back
    // `EXECUTE FUNCTION assert_row_not_still_declared(...)`.
    // `quote_all_identifiers` — with it on, every identifier comes back
    // double-quoted, down to `("new"."sidecar_tables")` inside the `WHEN`.
    //
    // Either one turns the comparison below into a refusal to boot a database
    // whose triggers are exactly right, and both are settable per-database and
    // per-role, so neither is hypothetical. Pinning them makes the rendering a
    // function of the trigger alone. `SET LOCAL` reverts when the transaction
    // ends, which is why the read takes one: the connection goes back to the
    // pool with the caller's settings intact.
    let mut tx = pool.begin().await.map_err(crate::error::map_err)?;
    for pin in [
        "SET LOCAL search_path = pg_catalog",
        "SET LOCAL quote_all_identifiers = off",
    ] {
        // SQL-POLICY: fixed-fragment — a literal from the array above.
        sqlx::query(pin)
            .execute(&mut *tx)
            .await
            .map_err(crate::error::map_err)?;
    }
    let found: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT (n.nspname || '.' || c.relname)::text,
                t.tgname::text,
                pg_get_triggerdef(t.oid)::text
           FROM pg_trigger t
           JOIN pg_class c ON c.oid = t.tgrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_proc p ON p.oid = t.tgfoid
           JOIN pg_namespace f ON f.oid = p.pronamespace
          WHERE NOT t.tgisinternal
            AND f.nspname = 'proxima_core'
            AND p.proname IN ('assert_memory_declares_sidecar',
                              'assert_declared_sidecar_present',
                              'assert_row_not_still_declared')
          ORDER BY 1, 2",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(crate::error::map_err)?;
    // Read-only, so there is nothing to keep; rolling back drops both pins
    // with it.
    tx.rollback().await.map_err(crate::error::map_err)?;

    let found_names: Vec<(&str, &str)> = found
        .iter()
        .map(|(relation, name, _)| (relation.as_str(), name.as_str()))
        .collect();
    let declared_names: Vec<(&str, &str)> = declared
        .iter()
        .map(|expected| (expected.relation.as_str(), expected.name.as_str()))
        .collect();
    if found_names != declared_names {
        return Err(StorageError::Internal(format!(
            "declaration triggers in the database do not match the registered memory sidecars: \
             registered {declared_names:?}, found {found_names:?}; apply migrations before \
             boot, and give every registered memory sidecar the triggers \
             `proxima_storage_pg::integrity`'s generators emit for it"
        )));
    }

    for (expected, (_, _, definition)) in declared.iter().zip(&found) {
        if definition != &expected.definition {
            return Err(StorageError::Internal(format!(
                "the trigger {} on {} is not the guard its registration declares;\n  \
                 registered: {}\n  found:      {definition}\nre-apply the trigger \
                 `proxima_storage_pg::integrity`'s generator emits for it",
                expected.name, expected.relation, expected.definition
            )));
        }
    }
    Ok(())
}

/// One projected `(sidecar table, schema)` pair the check looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedSchema {
    pub sidecar_table: String,
    pub schema_id: String,
    pub projection_table: String,
}

/// What the check looked at when it found nothing.
///
/// A report of an empty tree and a report of a tree the check could not
/// reach are different answers, and only the first one is worth asserting
/// on. The lists are what makes them distinguishable in a downstream CI
/// log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    /// Registered memory-sidecar tables checked against
    /// `memory.sidecar_tables`, in name order.
    pub declared_tables: Vec<String>,
    /// Projected schemas checked against their flavor's projection table,
    /// in `(table, schema)` order.
    pub projected_schemas: Vec<ProjectedSchema>,
}

/// One class of drift, with the count and the repair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityFinding {
    /// Sidecar rows of a projected schema with no projection row. Search
    /// cannot see them; the projection is what search ranks on.
    UnprojectedSidecarRows {
        sidecar_table: String,
        schema_id: String,
        projection_table: String,
        rows: i64,
    },
    /// Sidecar rows whose memory row does not declare the table they are
    /// in. Forget, owner erase and owner export cannot see them.
    UndeclaredSidecarRows { sidecar_table: String, rows: i64 },
    /// Memory rows that declare a table they have no row in. They cool into
    /// a cold object whose dump cannot equal its own stamp, so they can
    /// never be hydrated back.
    MissingStampedSidecarRows { sidecar_table: String, rows: i64 },
}

impl std::fmt::Display for IntegrityFinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnprojectedSidecarRows {
                sidecar_table,
                schema_id,
                projection_table,
                rows,
            } => write!(
                f,
                "{rows} row(s) of {schema_id} in {sidecar_table} have no row in \
                 {projection_table}, so lexical search cannot reach them. Repair: \
                 PgSidecarRegistryFrozen::rebuild_projection_for_table, which re-derives the \
                 projection row from the sidecar row that is already there"
            ),
            Self::UndeclaredSidecarRows {
                sidecar_table,
                rows,
            } => write!(
                f,
                "{rows} row(s) in {sidecar_table} belong to a memory that does not name that \
                 table in sidecar_tables, so forget, owner erase and owner export all walk past \
                 them. There is NO repair: the rows are outside every declaration, so nothing \
                 records which memory meant to declare them and no derivation can put them back \
                 — decide per row whether to delete it or to re-write it through \
                 Engine::unit_of_work"
            ),
            Self::MissingStampedSidecarRows {
                sidecar_table,
                rows,
            } => write!(
                f,
                "{rows} memory row(s) name {sidecar_table} in sidecar_tables and have no row \
                 in it. Forgetting one is refused at cool time, and one already cooled has a \
                 dump that cannot equal its stamp, so it can never be hydrated. There is NO \
                 repair from here: the payload the stamp promised was never written and \
                 nothing records what it should have said — decide per row whether to erase \
                 the Memory or to re-write it through Engine::unit_of_work"
            ),
        }
    }
}

/// The check's refusal. Never a warning: both classes are rows some lane
/// already walks past, and a warning is a lane that keeps walking past them.
#[derive(Debug, thiserror::Error)]
pub enum IntegrityViolation {
    #[error("declaration integrity check found {} drift(s):\n{}", .0.len(), render(.0))]
    Drift(Vec<IntegrityFinding>),
    #[error("the declaration integrity check could not run: {0}")]
    Storage(#[from] StorageError),
}

fn render(findings: &[IntegrityFinding]) -> String {
    findings
        .iter()
        .map(|finding| format!("  - {finding}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Rows of one projected schema whose projection row is missing. `$1` is the
/// schema id.
///
/// The `memory` join is not decoration: a sidecar table may be declared by
/// sibling registrations of more than one schema, and the projection
/// `INSERT` narrows on `m.schema_id = $3`. Counting without the join would
/// call every sibling's rows drift.
///
/// # Errors
///
/// [`StorageError::Internal`] for an invalid identifier.
pub(crate) fn unprojected_rows_sql(
    sidecar_table: &str,
    memory_key_column: &str,
    projection_table: &str,
) -> Result<String, StorageError> {
    let sidecar = PgIdent::table(sidecar_table)?;
    let projection = PgIdent::table(projection_table)?;
    let key = PgIdent::column(memory_key_column)?;
    // SQL-POLICY: PgIdent
    Ok(format!(
        "SELECT count(*)
  FROM {sidecar} c
  JOIN proxima_core.memory m ON m.t = c.{key}
 WHERE m.schema_id = $1
   AND NOT EXISTS (
           SELECT 1
             FROM {projection} p
            WHERE p.memory_id = c.{key}
              AND p.schema_id = $1
       )",
        sidecar = sidecar.as_str(),
        projection = projection.as_str(),
        key = key.as_str()
    ))
}

/// Rows of one registered memory-sidecar table that no memory declares.
/// `$1` is the table's own name.
///
/// # Errors
///
/// [`StorageError::Internal`] for an invalid identifier.
pub(crate) fn undeclared_rows_sql(
    sidecar_table: &str,
    memory_key_column: &str,
) -> Result<String, StorageError> {
    let sidecar = PgIdent::table(sidecar_table)?;
    let key = PgIdent::column(memory_key_column)?;
    // SQL-POLICY: PgIdent
    Ok(format!(
        "SELECT count(*)
  FROM {sidecar} c
 WHERE NOT EXISTS (
           SELECT 1
             FROM proxima_core.memory m
            WHERE m.t = c.{key}
              AND m.sidecar_tables @> ARRAY[$1::text]
       )",
        sidecar = sidecar.as_str(),
        key = key.as_str()
    ))
}

/// Memory rows that stamp one registered memory-sidecar table and have no
/// row in it. `$1` is the table's own name.
///
/// The mirror of [`undeclared_rows_sql`]. With both ends of direction 3
/// installed nothing can reach this state through `INSERT` or `DELETE`; the
/// read-back is for rows that predate the triggers, for a database whose
/// triggers someone dropped, and for the paths that suppress them at all
/// (`TRUNCATE`, `session_replication_role = replica`). Owner-pinned tables
/// are excluded by the caller for the reason
/// [`ensure_declaration_triggers`] gives.
///
/// # Errors
///
/// [`StorageError::Internal`] for an invalid identifier.
pub(crate) fn missing_stamped_rows_sql(
    sidecar_table: &str,
    memory_key_column: &str,
) -> Result<String, StorageError> {
    let sidecar = PgIdent::table(sidecar_table)?;
    let key = PgIdent::column(memory_key_column)?;
    // SQL-POLICY: PgIdent
    Ok(format!(
        "SELECT count(*)
  FROM proxima_core.memory m
 WHERE m.sidecar_tables @> ARRAY[$1::text]
   AND NOT EXISTS (
           SELECT 1
             FROM {sidecar} c
            WHERE c.{key} = m.t
       )",
        sidecar = sidecar.as_str(),
        key = key.as_str()
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        DECLARATION_TRIGGER_FUNCTION, DECLARATION_TRIGGER_FUNCTION_NAME, IntegrityFinding,
        IntegrityViolation, declaration_trigger, undeclared_rows_sql, unprojected_rows_sql,
    };

    #[test]
    fn the_trigger_reads_the_declared_key_column_and_carries_its_inverse() {
        let artifact = declaration_trigger("proxima_core.agent_note_v1", "t").expect("artifact");
        assert_eq!(
            artifact.forward,
            "CREATE OR REPLACE TRIGGER agent_note_v1_declared_by_memory
    BEFORE INSERT ON proxima_core.agent_note_v1
    FOR EACH ROW
    EXECUTE FUNCTION proxima_core.assert_memory_declares_sidecar('t');"
        );
        assert_eq!(
            artifact.inverse,
            "DROP TRIGGER IF EXISTS agent_note_v1_declared_by_memory \
             ON proxima_core.agent_note_v1;"
        );
    }

    /// The generator is a function of the DECLARATION, key column included.
    /// A literal `t` here would install a guard that reads NULL on every
    /// row of such a table and refuse the whole table's inserts.
    #[test]
    fn a_sidecar_keyed_on_another_column_is_guarded_on_that_column() {
        let artifact = declaration_trigger("public.notes_v1", "note_memory_id").expect("artifact");
        assert!(
            artifact
                .forward
                .contains("assert_memory_declares_sidecar('note_memory_id')"),
            "{}",
            artifact.forward
        );
    }

    /// A relation name that would push the derived trigger name past
    /// `PostgreSQL`'s 63 bytes is refused rather than silently truncated —
    /// truncation gives two tables one trigger name.
    #[test]
    fn a_trigger_name_that_would_be_truncated_is_refused() {
        let long = "s.".to_owned() + &"a".repeat(60);
        let err = declaration_trigger(&long, "t").expect_err("the name is too long");
        assert!(
            err.to_string().contains("declared_by_memory"),
            "the refusal names the identifier it built: {err}"
        );
    }

    #[test]
    fn the_shared_function_names_itself_once() {
        assert!(
            DECLARATION_TRIGGER_FUNCTION
                .contains(&format!("FUNCTION {DECLARATION_TRIGGER_FUNCTION_NAME}(")),
            "the constant and the body are one name"
        );
        assert!(
            DECLARATION_TRIGGER_FUNCTION.contains("TG_ARGV[0]"),
            "the key column arrives as a trigger argument"
        );
        assert!(
            DECLARATION_TRIGGER_FUNCTION.contains("TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME"),
            "the surface is where the trigger is installed, not a second declaration"
        );
    }

    #[test]
    fn the_projection_count_narrows_on_the_schema_and_the_declared_key() {
        let sql =
            unprojected_rows_sql("proxima_core.agent_note_v1", "t", "proxima_core.projection")
                .expect("sql");
        assert!(
            sql.contains("JOIN proxima_core.memory m ON m.t = c.t"),
            "{sql}"
        );
        assert!(sql.contains("WHERE m.schema_id = $1"), "{sql}");
        assert!(sql.contains("p.memory_id = c.t"), "{sql}");
    }

    #[test]
    fn the_declaration_count_binds_the_table_name() {
        let sql = undeclared_rows_sql("proxima_core.agent_note_v1", "t").expect("sql");
        assert!(sql.contains("m.sidecar_tables @> ARRAY[$1::text]"), "{sql}");
        assert!(
            !sql.contains("'proxima_core.agent_note_v1'"),
            "the table name is a bind, not a literal: {sql}"
        );
    }

    /// Both findings name what drifted and what to do about it, and the
    /// undeclared one says plainly that there is nothing to do.
    #[test]
    fn a_violation_names_the_table_the_count_and_the_repair() {
        let violation = IntegrityViolation::Drift(vec![
            IntegrityFinding::UnprojectedSidecarRows {
                sidecar_table: "proxima_core.agent_note_v1".to_owned(),
                schema_id: "core/agent-note-v1".to_owned(),
                projection_table: "proxima_core.projection".to_owned(),
                rows: 3,
            },
            IntegrityFinding::UndeclaredSidecarRows {
                sidecar_table: "proxima_core.utterance_v1".to_owned(),
                rows: 1,
            },
        ]);
        let message = violation.to_string();
        for named in [
            "proxima_core.agent_note_v1",
            "core/agent-note-v1",
            "proxima_core.projection",
            "rebuild_projection_for_table",
            "proxima_core.utterance_v1",
            "NO repair",
            "2 drift(s)",
        ] {
            assert!(message.contains(named), "{named} missing from: {message}");
        }
    }
}
