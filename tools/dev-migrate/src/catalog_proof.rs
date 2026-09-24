//! The catalog proof `--stamp` runs before it writes a ledger row.
//!
//! Stamping records migrations as applied **without executing them**, so it is
//! only honest when the live catalog already equals what the embedded lane
//! creates. Neither the ledger nor the structural markers can say that: after
//! d12da4f2 amended `0014_v015_owner_rls.sql` in place, a database that had
//! applied the v0.0.15 bytes (and then the pending v0.0.16 files by hand)
//! carried every marker, and stamping it recorded the amended checksum over
//! the ten old routine bodies — silent drift.
//!
//! The proof replays the embedded lane inside one transaction that is always
//! rolled back. The live `proxima_*` schemas are renamed aside first, so the
//! replay builds fresh schemas under the same names and one fingerprint query
//! reads both sides with identical text. Any difference refuses. Renaming and
//! creating a schema need `CREATE` on the database, as a fresh install does;
//! without it the proof fails and the stamp refuses.
//!
//! Compared, per schema the replay creates: routine definitions
//! (`pg_get_functiondef`: body, `SECURITY DEFINER`, `SET`, volatility),
//! aggregates, triggers (`pg_get_triggerdef` + enabled state), rules, policies
//! (command, permissive, roles with the table owner as `<table owner>`,
//! `USING`, `WITH CHECK`), relations (kind, persistence, options, RLS/FORCE
//! flags, columns, collations, defaults), constraints, indexes, views,
//! sequences, enum labels and domains. Not compared: owners and ACLs
//! (deployment state; the platform census checks table ownership) and rows
//! seeded by a migration. Objects a flavor this binary does not compose placed
//! on a replayed schema (its declaration triggers on `proxima_core` tables)
//! count as differences: such a database cannot be proven by this binary, so
//! the stamp refuses.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use sqlx::migrate::Migrator;
use sqlx::{PgConnection, PgPool};

/// The ledger the replay records into, in a schema created inside the rolled
/// back transaction (outside `proxima\_%`, so never fingerprinted): the replay
/// never reads or writes a live ledger, and needs no `TEMPORARY` privilege.
const SHADOW_LEDGER_SCHEMA: &str = "CREATE SCHEMA stamp_shadow_ledger";
const SHADOW_LEDGER: &str = "stamp_shadow_ledger._sqlx_migrations";

/// How many differences a refusal names before it summarises the rest.
const DIFFERENCES_SHOWN: usize = 40;

/// Every `proxima_*` schema moves to `stamp_live_<oid>` (outside the
/// `proxima\_%` namespace every census and migration scans) so the replay can
/// create the real names. One fixed literal: the names never become a Rust
/// format argument, and `format('%I')` is the server's own quoting.
const RENAME_LIVE_SCHEMAS_ASIDE: &str = r"DO $$
DECLARE live record;
BEGIN
    FOR live IN
        SELECT oid, nspname FROM pg_catalog.pg_namespace
         WHERE nspname LIKE 'proxima\_%' ESCAPE '\'
    LOOP
        EXECUTE format('ALTER SCHEMA %I RENAME TO %I', live.nspname, 'stamp_live_' || live.oid);
    END LOOP;
END $$";

/// One row per catalog object in a `proxima_*` schema:
/// `(schema, kind, name, definition)`. Run with `search_path = pg_catalog`, so
/// every deparsed name is schema-qualified the same way on both sides.
const FINGERPRINT: &str = r"
WITH scope AS (
    SELECT oid, nspname::text AS nspname FROM pg_catalog.pg_namespace
     WHERE nspname LIKE 'proxima\_%' ESCAPE '\'
), rel AS (
    SELECT c.oid, c.relname::text AS relname, c.relkind, s.nspname
      FROM pg_catalog.pg_class c JOIN scope s ON s.oid = c.relnamespace
)
SELECT s.nspname, 'routine', p.oid::regprocedure::text, pg_get_functiondef(p.oid)
  FROM pg_catalog.pg_proc p JOIN scope s ON s.oid = p.pronamespace
 WHERE p.prokind IN ('f', 'p')
UNION ALL
SELECT s.nspname, 'aggregate', p.oid::regprocedure::text,
       concat_ws(' ',
           'returns=' || pg_get_function_result(p.oid),
           'kind=' || ag.aggkind::text, 'strict=' || p.proisstrict,
           'parallel=' || p.proparallel::text,
           'transfn=' || ag.aggtransfn::regprocedure::text,
           'finalfn=' || ag.aggfinalfn::regprocedure::text,
           'combinefn=' || ag.aggcombinefn::regprocedure::text,
           'serialfn=' || ag.aggserialfn::regprocedure::text,
           'deserialfn=' || ag.aggdeserialfn::regprocedure::text,
           'mtransfn=' || ag.aggmtransfn::regprocedure::text,
           'stype=' || format_type(ag.aggtranstype, NULL),
           'initval=' || COALESCE(ag.agginitval, '<null>'),
           'sortop=' || ag.aggsortop::regoperator::text)
  FROM pg_catalog.pg_proc p
  JOIN scope s ON s.oid = p.pronamespace
  JOIN pg_catalog.pg_aggregate ag ON ag.aggfnoid = p.oid
 WHERE p.prokind = 'a'
UNION ALL
SELECT rel.nspname, 'relation', rel.relname,
       concat_ws(E'\n',
           'kind=' || c.relkind::text || ' persistence=' || c.relpersistence::text
               || ' rls=' || c.relrowsecurity || ' force_rls=' || c.relforcerowsecurity
               || ' options=' || COALESCE(array_to_string(c.reloptions, ','), ''),
           (SELECT string_agg(
                       a.attname || ' ' || format_type(a.atttypid, a.atttypmod)
                       || CASE WHEN a.attnotnull THEN ' not null' ELSE '' END
                       || COALESCE(' default ' || pg_get_expr(d.adbin, d.adrelid), '')
                       || CASE WHEN a.attidentity <> '' THEN ' identity ' || a.attidentity::text ELSE '' END
                       || CASE WHEN a.attgenerated <> '' THEN ' generated ' || a.attgenerated::text ELSE '' END
                       || CASE WHEN a.attcollation <> 0
                                    AND a.attcollation <> (SELECT ty.typcollation FROM pg_catalog.pg_type ty
                                                            WHERE ty.oid = a.atttypid)
                               THEN ' collate ' || a.attcollation::regcollation::text ELSE '' END,
                       E'\n' ORDER BY a.attnum)
              FROM pg_catalog.pg_attribute a
              LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
             WHERE a.attrelid = rel.oid AND a.attnum > 0 AND NOT a.attisdropped))
  FROM rel JOIN pg_catalog.pg_class c ON c.oid = rel.oid
 WHERE rel.relkind IN ('r', 'p', 'v', 'm', 'f', 'c')
UNION ALL
SELECT rel.nspname, 'view', rel.relname, pg_get_viewdef(rel.oid)
  FROM rel WHERE rel.relkind IN ('v', 'm')
UNION ALL
SELECT rel.nspname, 'constraint', rel.relname || '.' || con.conname, pg_get_constraintdef(con.oid)
  FROM pg_catalog.pg_constraint con JOIN rel ON rel.oid = con.conrelid
UNION ALL
SELECT rel.nspname, 'index', ic.relname::text, pg_get_indexdef(ix.indexrelid)
  FROM pg_catalog.pg_index ix
  JOIN pg_catalog.pg_class ic ON ic.oid = ix.indexrelid
  JOIN rel ON rel.oid = ix.indrelid
UNION ALL
SELECT rel.nspname, 'trigger', rel.relname || '.' || t.tgname,
       pg_get_triggerdef(t.oid) || ' enabled=' || t.tgenabled::text
  FROM pg_catalog.pg_trigger t JOIN rel ON rel.oid = t.tgrelid
 WHERE NOT t.tgisinternal
UNION ALL
SELECT rel.nspname, 'rule', rel.relname || '.' || ru.rulename,
       pg_get_ruledef(ru.oid) || ' enabled=' || ru.ev_enabled::text
  FROM pg_catalog.pg_rewrite ru JOIN rel ON rel.oid = ru.ev_class
 WHERE ru.rulename <> '_RETURN'
UNION ALL
SELECT rel.nspname, 'policy', rel.relname || '.' || pol.polname,
       concat_ws(E'\n',
           'command=' || pol.polcmd::text || ' permissive=' || pol.polpermissive,
           'roles=' || (SELECT string_agg(role_name, ',' ORDER BY role_name)
                          FROM (SELECT CASE WHEN r.oid = 0 THEN 'public'
                                            WHEN r.oid = pc.relowner THEN '<table owner>'
                                            ELSE r.oid::regrole::text END AS role_name
                                  FROM unnest(pol.polroles) AS r(oid)) AS roles),
           'using=' || COALESCE(pg_get_expr(pol.polqual, pol.polrelid), ''),
           'check=' || COALESCE(pg_get_expr(pol.polwithcheck, pol.polrelid), ''))
  FROM pg_catalog.pg_policy pol
  JOIN rel ON rel.oid = pol.polrelid
  JOIN pg_catalog.pg_class pc ON pc.oid = pol.polrelid
UNION ALL
SELECT rel.nspname, 'sequence', rel.relname,
       format_type(q.seqtypid, NULL) || ' start=' || q.seqstart || ' increment=' || q.seqincrement
           || ' min=' || q.seqmin || ' max=' || q.seqmax || ' cycle=' || q.seqcycle
  FROM pg_catalog.pg_sequence q JOIN rel ON rel.oid = q.seqrelid
UNION ALL
SELECT s.nspname, 'type', t.typname::text,
       CASE t.typtype
           WHEN 'e' THEN 'enum ' || (SELECT string_agg(e.enumlabel, ',' ORDER BY e.enumsortorder)
                                       FROM pg_catalog.pg_enum e WHERE e.enumtypid = t.oid)
           ELSE 'domain ' || format_type(t.typbasetype, t.typtypmod)
               || CASE WHEN t.typnotnull THEN ' not null' ELSE '' END
               || COALESCE(' default ' || t.typdefault, '')
               || COALESCE((SELECT ' ' || string_agg(pg_get_constraintdef(dc.oid), ' ' ORDER BY dc.conname)
                              FROM pg_catalog.pg_constraint dc WHERE dc.contypid = t.oid), '')
       END
  FROM pg_catalog.pg_type t JOIN scope s ON s.oid = t.typnamespace
 WHERE t.typtype IN ('e', 'd')";

/// `(schema, kind, name) -> definition`.
type Fingerprint = BTreeMap<(String, String, String), String>;

/// Refuse unless the live catalog equals what `lanes` create on an empty
/// database. Nothing the proof does survives it: the replay and the renames
/// roll back, and the connection is closed rather than returned to the pool.
///
/// # Errors
///
/// Every catalog difference, named; or the replay's own failure (a lane that
/// cannot replay cannot be proven, so the stamp refuses).
pub(crate) async fn refuse_unless_live_catalog_matches(
    pool: &PgPool,
    lanes: &[&Migrator],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut connection = pool.acquire().await?.detach();
    // The replay is a full lane build: no request-serving timeout may cut it
    // off, and a lock it cannot take fails fast like a boot migration does.
    sqlx::raw_sql("SET statement_timeout = 0; SET lock_timeout = '5s'")
        .execute(&mut connection)
        .await?;
    let outcome = replay_and_compare(&mut connection, lanes).await;
    // Closing ends the session, so the server discards anything still open.
    drop(connection);
    let differences = outcome.map_err(|error| {
        format!(
            "refusing --stamp: could not prove the live catalog matches the embedded \
             migrations (replaying them in a rolled-back transaction failed): {error}. The \
             proof renames the live proxima_* schemas aside and builds them afresh, so the \
             stamping role needs CREATE on the database, as for a fresh install"
        )
    })?;
    if differences.is_empty() {
        return Ok(());
    }
    // The detail goes to stderr line by line; the error names the objects
    // on one line, since `main` prints it through `Debug`.
    eprintln!("the live catalog differs from what the embedded migrations create:");
    for (object, detail) in &differences {
        eprintln!("  - {object}: {detail}");
    }
    let mut named: Vec<&str> = differences
        .iter()
        .take(DIFFERENCES_SHOWN)
        .map(|(object, _)| object.as_str())
        .collect();
    let more = format!(
        "and {} more",
        differences.len().saturating_sub(DIFFERENCES_SHOWN)
    );
    if differences.len() > DIFFERENCES_SHOWN {
        named.push(&more);
    }
    Err(format!(
        "refusing --stamp: the live catalog differs from what the embedded migrations create \
         ({} difference(s): {}), so recording them as applied would hide the drift. A migration \
         amended after this database applied it leaves the old objects in place; restore the \
         database from before that migration and re-run it, or reset (dev/staging only). See \
         docs/how-to/migrations.md",
        differences.len(),
        named.join("; ")
    )
    .into())
}

async fn replay_and_compare(
    connection: &mut PgConnection,
    lanes: &[&Migrator],
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    // The same transaction a composed migration run opens: platform scope,
    // and the census that refuses a role that does not own the schema.
    let mut transaction = proxima_storage_pg::begin_migration_transaction(connection).await?;
    let live = fingerprint(&mut transaction).await?;

    sqlx::query("SELECT set_config('search_path', 'public', true)")
        .execute(&mut *transaction)
        .await?;
    sqlx::raw_sql(RENAME_LIVE_SCHEMAS_ASIDE)
        .execute(&mut *transaction)
        .await?;
    sqlx::raw_sql(SHADOW_LEDGER_SCHEMA)
        .execute(&mut *transaction)
        .await?;
    for lane in lanes {
        let mut shadow = Migrator {
            migrations: lane.migrations.clone(),
            create_schemas: lane.create_schemas.clone(),
            ..Migrator::DEFAULT
        };
        shadow
            .set_locking(false)
            .set_ignore_missing(true)
            .dangerous_set_table_name(Cow::Borrowed(SHADOW_LEDGER));
        shadow.run_direct(None, &mut *transaction, false).await?;
    }
    let replayed = fingerprint(&mut transaction).await?;
    transaction.rollback().await?;
    Ok(differences(&live, &replayed))
}

/// Read the catalog with `search_path = pg_catalog` (transaction-local; the
/// caller resets it to `public` before replaying).
async fn fingerprint(
    connection: &mut PgConnection,
) -> Result<Fingerprint, Box<dyn std::error::Error>> {
    sqlx::query("SELECT set_config('search_path', 'pg_catalog', true)")
        .execute(&mut *connection)
        .await?;
    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(FINGERPRINT)
        .fetch_all(&mut *connection)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(schema, kind, name, definition)| {
            ((schema, kind, name), definition.unwrap_or_default())
        })
        .collect())
}

/// Differences between the live catalog and the replay, as `(object,
/// detail)`, limited to the schemas the replay created: a whole schema no
/// stamped lane owns is not this stamp's claim, but anything inside a
/// replayed schema is, whoever put it there.
fn differences(live: &Fingerprint, replayed: &Fingerprint) -> Vec<(String, String)> {
    let schemas: BTreeSet<&String> = replayed.keys().map(|(schema, _, _)| schema).collect();
    let mut differences = Vec::new();
    for (key, embedded) in replayed {
        let (_, kind, name) = key;
        match live.get(key) {
            None => differences.push((
                format!("{kind} {name}"),
                "missing from the live database".to_owned(),
            )),
            Some(actual) if actual != embedded => differences.push((
                format!("{kind} {name}"),
                format!(
                    "differs from the embedded migrations ({})",
                    first_difference(actual, embedded)
                ),
            )),
            Some(_) => {}
        }
    }
    for key in live.keys() {
        let (schema, kind, name) = key;
        if schemas.contains(schema) && !replayed.contains_key(key) {
            differences.push((
                format!("{kind} {name}"),
                "in the live database but not created by the embedded migrations".to_owned(),
            ));
        }
    }
    differences
}

/// The first differing line, so an operator sees *what* drifted.
fn first_difference(live: &str, embedded: &str) -> String {
    const SHOWN: usize = 120;
    let clip = |line: &str| line.trim().chars().take(SHOWN).collect::<String>();
    let mut live_lines = live.lines();
    let mut embedded_lines = embedded.lines();
    let mut number = 1;
    loop {
        match (live_lines.next(), embedded_lines.next()) {
            (Some(left), Some(right)) if left == right => number += 1,
            (left, right) => {
                return format!(
                    "line {number}: live `{}`, embedded `{}`",
                    left.map_or_else(|| "<end>".to_owned(), clip),
                    right.map_or_else(|| "<end>".to_owned(), clip),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Fingerprint, differences, first_difference};

    fn entry(
        schema: &str,
        kind: &str,
        name: &str,
        definition: &str,
    ) -> ((String, String, String), String) {
        (
            (schema.to_owned(), kind.to_owned(), name.to_owned()),
            definition.to_owned(),
        )
    }

    #[test]
    fn differences_name_missing_changed_and_extra_objects_in_replayed_schemas_only() {
        let replayed: Fingerprint = [
            entry("proxima_core", "routine", "f()", "BEGIN\nnew\nEND"),
            entry("proxima_core", "trigger", "memory.t", "CREATE TRIGGER t"),
        ]
        .into_iter()
        .collect();
        let live: Fingerprint = [
            entry("proxima_core", "routine", "f()", "BEGIN\nold\nEND"),
            entry("proxima_core", "policy", "memory.p", "using=true"),
            entry("proxima_other", "routine", "g()", "not this stamp's claim"),
        ]
        .into_iter()
        .collect();
        let rendered: Vec<String> = differences(&live, &replayed)
            .into_iter()
            .map(|(object, detail)| format!("{object}: {detail}"))
            .collect();
        assert_eq!(
            rendered,
            vec![
                "routine f(): differs from the embedded migrations (line 2: live `old`, embedded `new`)",
                "trigger memory.t: missing from the live database",
                "policy memory.p: in the live database but not created by the embedded migrations",
            ]
        );
        assert!(differences(&replayed, &replayed).is_empty());
    }

    #[test]
    fn first_difference_reports_a_missing_trailing_line() {
        assert_eq!(
            first_difference("a\nb", "a\nb\nc"),
            "line 3: live `<end>`, embedded `c`"
        );
    }
}
