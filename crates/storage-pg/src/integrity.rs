//! Declaration integrity: the floor under every write path, and the check
//! that reads it back.
//!
//! One invariant, stated twice. **A row in a registered memory-sidecar
//! table exists only for a memory row whose `sidecar_tables` declares that
//! table.** `assert_sidecar_stamp_declared` (`0001_v008.sql`) already holds
//! the other direction — a stamp names only tables some flavor declares —
//! and the two together are the array foreign key `PostgreSQL` has no
//! syntax for.
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
//! 1. **The generator.** [`DECLARATION_TRIGGER_FUNCTION`] is fixed text and
//!    [`declaration_trigger`] emits one `CREATE OR REPLACE TRIGGER` per
//!    registered memory-sidecar table, each carrying its own `DROP`. Both
//!    are pasted verbatim into the v0.0.9 migrations —
//!    `0002_v009_declaration_triggers.sql` and each flavor's own — and pinned
//!    by `generated_declaration_triggers_are_the_migration_text`, so the
//!    migration author cannot drift from the generator. Deliberately NOT into
//!    the v0.0.8 baselines: those are frozen, and editing one would change
//!    the checksum of a version live databases have already applied, turning
//!    an additive release into a forced reset (docs/how-to/migrations.md).
//! 2. **The boot guardrail** ([`ensure_declaration_triggers`]), which re-runs
//!    the generator against the frozen registry and compares it with
//!    `pg_trigger`. It issues no DDL: in the split-role topology (docs/15) an
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
    let (_, relation) = sidecar_table.split_once('.').ok_or_else(|| {
        StorageError::Internal(format!(
            "a registered sidecar table must be schema-qualified: {sidecar_table:?}"
        ))
    })?;
    let name = format!("{relation}_{DECLARATION_TRIGGER_SUFFIX}");
    let trigger = PgIdent::column(&name)?;
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
        trigger = trigger.as_str(),
        table = table.as_str(),
        key = key.as_str()
    );
    // SQL-POLICY: PgIdent
    let inverse = format!(
        "DROP TRIGGER IF EXISTS {} ON {};",
        trigger.as_str(),
        table.as_str()
    );
    Ok(Artifact { forward, inverse })
}

/// Every declaration trigger the linked flavors expect, as `pg_trigger` sees
/// it — and nothing else.
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
/// memory-key column than its registration declares, or when a registered
/// memory sidecar declares no memory-key column at all.
pub async fn ensure_declaration_triggers(
    pool: &sqlx::PgPool,
    registry: &PgSidecarRegistryFrozen,
) -> Result<(), StorageError> {
    let mut declared: Vec<(String, String)> = Vec::new();
    for table in registry.memory_sidecar_tables() {
        let key = registry.memory_key_column(table).ok_or_else(|| {
            StorageError::Internal(format!(
                "{table} is a registered memory sidecar with no declared memory-key column; \
                 give its `pg_sidecar!` registration a `key:` and its contract a \
                 KeyShape::MemoryT {{ column }}"
            ))
        })?;
        declared.push((table.to_owned(), key.to_owned()));
    }
    declared.sort();

    // `pg_get_triggerdef` renders the argument list back as `SQL`, so the
    // key column a trigger actually runs on is readable without decoding
    // `pg_trigger.tgargs`' NUL-separated bytea by hand.
    let found: Vec<(String, String)> = sqlx::query_as(
        "SELECT (n.nspname || '.' || c.relname)::text,
                pg_get_triggerdef(t.oid)::text
           FROM pg_trigger t
           JOIN pg_class c ON c.oid = t.tgrelid
           JOIN pg_namespace n ON n.oid = c.relnamespace
           JOIN pg_proc p ON p.oid = t.tgfoid
           JOIN pg_namespace f ON f.oid = p.pronamespace
          WHERE NOT t.tgisinternal
            AND f.nspname = 'proxima_core'
            AND p.proname = 'assert_memory_declares_sidecar'
          ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .map_err(crate::error::map_err)?;

    let found_tables: Vec<&str> = found.iter().map(|(table, _)| table.as_str()).collect();
    let declared_tables: Vec<&str> = declared.iter().map(|(table, _)| table.as_str()).collect();
    if found_tables != declared_tables {
        return Err(StorageError::Internal(format!(
            "declaration triggers in the database do not match the registered memory sidecars: \
             registered {declared_tables:?}, found {found_tables:?}; apply migrations before \
             boot, and give every registered memory sidecar the trigger \
             `proxima_storage_pg::integrity::declaration_trigger` emits for it"
        )));
    }

    for ((table, key), (_, definition)) in declared.iter().zip(&found) {
        let expected = format!("{DECLARATION_TRIGGER_FUNCTION_NAME}('{key}')");
        if !definition.contains(&expected) {
            return Err(StorageError::Internal(format!(
                "the declaration trigger on {table} does not read the declared memory-key \
                 column {key:?}: {definition}; re-apply the trigger \
                 `proxima_storage_pg::integrity::declaration_trigger` emits for it"
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
