//! Fixed, contract-derived candidate functions carried by migrations.
//!
//! A function accepts search values and narrowing filters, never SQL or a
//! relation name. Its private execution role can scan the derived projection;
//! ordinary memory RLS authorizes every id before the function returns it.

use proxima_core::StorageError;
use proxima_core::flavor::FlavorContract;

use crate::pg_ident::PgIdent;
use crate::projection::Artifact;
use crate::verbs::query::SubstringCandidates;

/// The fixed `PostgreSQL` identity arguments of both candidate functions.
pub const ARGUMENT_TYPES: &str = "text, bigint, text[], timestamp with time zone, timestamp with time zone, uuid, uuid[], text[], boolean, boolean, boolean, text, boolean";

const PARAMETERS: &str = "query_text text, candidate_limit bigint, requested_tags text[], since_time timestamptz, until_time timestamptz, before_id uuid, requested_owners uuid[], requested_schemas text[], rescue boolean, recency boolean, heads_only boolean, requested_kind text, tag_all boolean";

/// Qualified function name derived from a projection's schema.
///
/// # Errors
/// Refuses a projection table that is not a qualified `PostgreSQL` identifier.
pub fn function_name(table: &str, substring: bool) -> Result<String, StorageError> {
    let _ = PgIdent::table(table)?;
    let (schema, _) = table.split_once('.').ok_or_else(|| {
        StorageError::Internal("candidate projection must be schema-qualified".into())
    })?;
    let name = if substring {
        "search_substring_candidates"
    } else {
        "search_lexical_candidates"
    };
    let qualified = format!("{schema}.{name}");
    let _ = PgIdent::table(&qualified)?;
    Ok(qualified)
}

pub(crate) fn call_sql(table: &str, substring: bool) -> Result<String, StorageError> {
    let name = function_name(table, substring)?;
    let columns = if substring {
        "t, lexical_score"
    } else {
        "t, schema_id, lexical_score"
    };
    // SQL-POLICY: PgIdent — the function name is derived from the checked schema.
    Ok(format!(
        "SELECT {columns} FROM {name}(\
         $1::text, $2::bigint, $3::text[], $4::timestamptz, $5::timestamptz, \
         $6::uuid, $7::uuid[], $8::text[], $9::boolean, $10::boolean, \
         $11::boolean, $12::text, $13::boolean)"
    ))
}

/// Generate the lexical and declared substring functions for one flavor.
///
/// Migration files carry these artifacts. Serving boot only verifies them;
/// this function never accesses a database or installs objects at runtime.
///
/// # Errors
/// Refuses invalid declared identifiers or inconsistent search projections.
pub fn candidate_artifacts(
    contract: &'static FlavorContract,
) -> Result<Vec<Artifact>, StorageError> {
    let Some(spec) = contract.projection.spec() else {
        return Ok(Vec::new());
    };
    let projections = proxima_core::verbs::schema::contract_search_projections(&[contract])
        .map_err(|error| StorageError::Internal(error.to_string()))?;
    candidate_artifacts_for_projection(spec.table, spec.overfetch_k, &projections)
}

/// Candidate artifacts for one synthetic projection used by database fixtures.
///
/// # Errors
/// Refuses invalid identifiers or incomplete fixture metadata.
#[cfg(any(test, feature = "test-fixtures", debug_assertions))]
#[doc(hidden)]
pub fn candidate_artifacts_for_tests(
    projections: &[proxima_core::verbs::schema::MemorySearchProjection],
) -> Result<Vec<Artifact>, StorageError> {
    let Some(head) = projections.first() else {
        return Ok(Vec::new());
    };
    candidate_artifacts_for_projection(&head.projection_table, head.overfetch_k, projections)
}

fn candidate_artifacts_for_projection(
    table: &str,
    cap: u32,
    projections: &[proxima_core::verbs::schema::MemorySearchProjection],
) -> Result<Vec<Artifact>, StorageError> {
    let mut artifacts = Vec::new();
    for substring in [false, true] {
        let (relevance, recency) = if substring {
            let Some(relevance) =
                crate::verbs::query::projection_substring_candidates(projections, false)?
            else {
                continue;
            };
            let recency = crate::verbs::query::projection_substring_candidates(projections, true)?
                .ok_or_else(missing_recency)?;
            (
                substring_execution(&relevance),
                substring_execution(&recency),
            )
        } else {
            let Some(relevance) =
                crate::verbs::query::projection_candidate_statements(projections, false, false)?
            else {
                continue;
            };
            let recency =
                crate::verbs::query::projection_candidate_statements(projections, false, true)?
                    .ok_or_else(missing_recency)?;
            (execution(&relevance), execution(&recency))
        };
        let name = function_name(table, substring)?;
        let returns = if substring {
            "t uuid, lexical_score real"
        } else {
            "t uuid, schema_id text, lexical_score real"
        };
        // SQL-POLICY: PgIdent — name and all body identifiers are checked by the renderer.
        let forward = format!(
            "CREATE OR REPLACE FUNCTION {name}({PARAMETERS})\n\
             RETURNS TABLE({returns})\n\
             LANGUAGE plpgsql SECURITY DEFINER\n\
             SET search_path TO pg_catalog, pg_temp\n\
             AS $candidate_function$\n\
             BEGIN\n\
                 PERFORM proxima_core.assert_search_scope(requested_owners);\n\
                 IF candidate_limit IS NULL OR candidate_limit < 1 OR candidate_limit > {cap} THEN\n\
                     RAISE EXCEPTION 'candidate limit must be between 1 and {cap}' USING ERRCODE = '22023';\n\
                 END IF;\n\
                 IF recency THEN\n\
                     {recency}\n\
                 ELSE\n\
                     {relevance}\n\
                 END IF;\n\
             END\n\
             $candidate_function$;",
        );
        let signature = format!("{name}({ARGUMENT_TYPES})");
        let mut read_tables = vec![
            "proxima_core.memory",
            "proxima_core.memory_head",
            "proxima_core.lexical_languages",
            "proxima_core.lexical_default",
            table,
        ];
        read_tables.extend(
            projections
                .iter()
                .map(|projection| projection.sidecar_table.as_str()),
        );
        read_tables.sort_unstable();
        read_tables.dedup();
        let schema = table
            .split_once('.')
            .ok_or_else(|| StorageError::Internal("unqualified projection".into()))?
            .0;
        let forward = installation_sql(schema, &forward, &signature, &read_tables, &[table])?;
        artifacts.push(Artifact {
            forward,
            // SQL-POLICY: PgIdent — the inverse uses the same checked function name.
            inverse: format!("DROP FUNCTION IF EXISTS {name}({ARGUMENT_TYPES});"),
        });
    }
    Ok(artifacts)
}

fn missing_recency() -> StorageError {
    StorageError::Internal("candidate arm has no recency statement".into())
}

const USING_ARGUMENTS: &str = "USING query_text, candidate_limit, requested_tags, since_time, \
     until_time, before_id, requested_owners, requested_schemas, rescue, heads_only, \
     requested_kind, tag_all;";

fn quoted(sql: &str) -> String {
    sql.replace('\'', "''")
}

/// EXECUTE plans each search using its actual values. A generic plan cannot
/// hide the tag/text condition behind optional-filter switches.
fn execution(sql: &str) -> String {
    format!("RETURN QUERY EXECUTE '{}' {USING_ARGUMENTS}", quoted(sql))
}

/// The substring arm plans one statement per request over only the legs whose
/// schemas the request names: each leg is `m.schema_id = ANY($8)` and
/// `= ANY(<its own schemas>)`, so a leg the request does not name matches no
/// row, and planning it (about 2 ms of row-security expansion) buys nothing.
fn substring_execution(candidates: &SubstringCandidates) -> String {
    let (open, between, close) = candidates.frame();
    let legs = candidates
        .legs
        .iter()
        .map(|leg| {
            let schemas = leg
                .schemas
                .iter()
                .map(|schema| format!("'{}'", schema.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "IF requested_schemas && ARRAY[{schemas}]::text[] THEN \
                 legs := legs || '{}'::text; END IF;",
                quoted(&leg.sql)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "DECLARE legs text[] := ARRAY[]::text[];\n\
         BEGIN\n{legs}\n\
         IF cardinality(legs) = 0 THEN RETURN; END IF;\n\
         RETURN QUERY EXECUTE '{}' || array_to_string(legs, '{}') || '{}' {USING_ARGUMENTS}\n\
         END;",
        quoted(open),
        quoted(between),
        quoted(&close),
    )
}

/// Install only generated functions while assuming the non-inheritable owner.
/// CREATE is temporary; subsequent definition passes use the same path.
pub(crate) fn installation_sql(
    schema: &str,
    function_ddl: &str,
    signature: &str,
    read_tables: &[&str],
    candidate_tables: &[&str],
) -> Result<String, StorageError> {
    let schema = PgIdent::column(schema)?;
    let read_tables = read_tables
        .iter()
        .map(|table| PgIdent::table(table))
        .collect::<Result<Vec<_>, _>>()?;
    let grants = read_tables
        .iter()
        .map(|table| table.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let policies = candidate_tables.iter().map(|table| {
        let table = PgIdent::table(table)?;
        // SQL-POLICY: PgIdent — the candidate table is contract-derived.
        Ok(format!(
            "DROP POLICY IF EXISTS proxima_search_candidates ON {table};\n\
             CREATE POLICY proxima_search_candidates ON {table} FOR SELECT TO proxima_search USING (true);",
            table = table.as_str()
        ))
    }).collect::<Result<Vec<_>, StorageError>>()?.join("\n");
    // SQL-POLICY: PgIdent — checked schema/tables; function DDL and signature are private generated fragments.
    Ok(format!(
        "DO $install_candidate$\n\
         DECLARE original_role name := current_user;\n\
         BEGIN\n\
             GRANT USAGE, CREATE ON SCHEMA {schema} TO proxima_search;\n\
             GRANT SELECT ON {grants} TO proxima_search;\n\
             {policies}\n\
             SET LOCAL ROLE proxima_search;\n\
             EXECUTE $generated_function${function_ddl}$generated_function$;\n\
             REVOKE ALL ON FUNCTION {signature} FROM PUBLIC;\n\
             EXECUTE format('SET LOCAL ROLE %I', original_role);\n\
             REVOKE CREATE ON SCHEMA {schema} FROM proxima_search;\n\
         END\n\
         $install_candidate$;",
        schema = schema.as_str(),
    ))
}
