use proxima_core::compliance::{
    ComplianceExportBundle, ComplianceExportCounts, ComplianceExportSidecarRows,
    ExportAuthorization,
};
use proxima_core::{OwnerRef, StorageError};
use serde_json::Value;
use sqlx::PgPool;

use crate::access::owner_columns::owner_binds;
use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::compliance_erase::owner_digest;

pub async fn export_owner_bundle(
    pool: &PgPool,
    auth: &ExportAuthorization,
    fact_sidecar_tables: &[String],
    goal_sidecar_tables: &[String],
    citation_mapping_sidecar_tables: &[String],
    cited_object_sidecar_tables: &[String],
) -> Result<ComplianceExportBundle, StorageError> {
    let owner = auth.audit().owner();
    let memories = owner_rows(pool, owner, OwnerRowsTable::Memories).await?;
    let goals = owner_rows(pool, owner, OwnerRowsTable::Goals).await?;
    let edges = owner_rows(pool, owner, OwnerRowsTable::Edges).await?;
    let receipts = owner_rows(pool, owner, OwnerRowsTable::Receipts).await?;
    let source_batches = owner_rows(pool, owner, OwnerRowsTable::SourceBatches).await?;
    let source_cursors = owner_rows(pool, owner, OwnerRowsTable::SourceCursors).await?;
    let delegated_authority_grants =
        owner_rows(pool, owner, OwnerRowsTable::DelegatedAuthorityGrants).await?;
    let compliance_audit_rows = audit_rows(pool, owner).await?;
    let sidecars = export_sidecars(
        pool,
        owner,
        SidecarTables {
            fact: fact_sidecar_tables,
            goal: goal_sidecar_tables,
            citation_mapping: citation_mapping_sidecar_tables,
            cited_object: cited_object_sidecar_tables,
        },
    )
    .await?;

    let counts = ComplianceExportCounts {
        memories: memories.len(),
        goals: goals.len(),
        edges: edges.len(),
        receipts: receipts.len(),
        source_batches: source_batches.len(),
        source_cursors: source_cursors.len(),
        delegated_authority_grants: delegated_authority_grants.len(),
        sidecar_rows: sidecars.iter().map(|sidecar| sidecar.rows.len()).sum(),
        compliance_audit_rows: compliance_audit_rows.len(),
    };

    Ok(ComplianceExportBundle {
        operation_id: auth.audit().operation_id(),
        target: auth.audit().target().clone(),
        owner,
        derived_requester: auth.audit().derived_requester(),
        derived_auth_path: format!("{:?}", auth.audit().derived_auth_path()),
        exported_at: auth.audit().requested_at(),
        counts,
        memories,
        goals,
        edges,
        receipts,
        source_batches,
        source_cursors,
        delegated_authority_grants,
        sidecars,
        compliance_audit_rows,
    })
}

struct SidecarTables<'a> {
    fact: &'a [String],
    goal: &'a [String],
    citation_mapping: &'a [String],
    cited_object: &'a [String],
}

#[derive(Clone, Copy)]
enum OwnerRowsTable {
    Memories,
    Goals,
    Edges,
    Receipts,
    SourceBatches,
    SourceCursors,
    DelegatedAuthorityGrants,
}

async fn export_sidecars(
    pool: &PgPool,
    owner: OwnerRef,
    tables: SidecarTables<'_>,
) -> Result<Vec<ComplianceExportSidecarRows>, StorageError> {
    let mut sidecars = Vec::new();
    extend_sidecars(
        pool,
        owner,
        &mut sidecars,
        tables.fact,
        SidecarJoin {
            sidecar_column: "memory_id",
            base_table: "proxima_core.memories",
            base_column: "memory_id",
        },
    )
    .await?;
    extend_sidecars(
        pool,
        owner,
        &mut sidecars,
        tables.goal,
        SidecarJoin {
            sidecar_column: "goal_id",
            base_table: "proxima_core.goals",
            base_column: "goal_id",
        },
    )
    .await?;
    let _ = tables.citation_mapping;
    let _ = tables.cited_object;
    sidecars.sort_by(|left, right| left.table.cmp(&right.table));
    Ok(sidecars)
}

async fn owner_rows(
    pool: &PgPool,
    owner: OwnerRef,
    table: OwnerRowsTable,
) -> Result<Vec<Value>, StorageError> {
    let (owner_kind, owner_id) = owner_binds(&owner);
    match table {
        OwnerRowsTable::Memories => sqlx::query_scalar::<_, Value>(MEMORY_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::Goals => sqlx::query_scalar::<_, Value>(GOAL_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::Edges => sqlx::query_scalar::<_, Value>(EDGE_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::Receipts => sqlx::query_scalar::<_, Value>(RECEIPT_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::SourceBatches => sqlx::query_scalar::<_, Value>(SOURCE_BATCH_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::SourceCursors => sqlx::query_scalar::<_, Value>(SOURCE_CURSOR_ROWS_SQL)
            .bind(owner_kind)
            .bind(owner_id)
            .fetch_all(pool)
            .await
            .map_err(map_err),
        OwnerRowsTable::DelegatedAuthorityGrants => {
            sqlx::query_scalar::<_, Value>(DELEGATED_AUTHORITY_GRANT_ROWS_SQL)
                .bind(owner_kind)
                .bind(owner_id)
                .fetch_all(pool)
                .await
                .map_err(map_err)
        }
    }
}

async fn audit_rows(pool: &PgPool, owner: OwnerRef) -> Result<Vec<Value>, StorageError> {
    sqlx::query_scalar::<_, Value>(
        "SELECT to_jsonb(a)
           FROM proxima_core.compliance_audit_log a
          WHERE a.owner_ref_digest = $1
          ORDER BY a.requested_at, a.operation_id",
    )
    .bind(owner_digest(owner))
    .fetch_all(pool)
    .await
    .map_err(map_err)
}

#[derive(Clone, Copy)]
struct SidecarJoin {
    sidecar_column: &'static str,
    base_table: &'static str,
    base_column: &'static str,
}

async fn extend_sidecars(
    pool: &PgPool,
    owner: OwnerRef,
    sidecars: &mut Vec<ComplianceExportSidecarRows>,
    tables: &[String],
    join: SidecarJoin,
) -> Result<(), StorageError> {
    for table in tables {
        let rows = sidecar_rows(pool, owner, table, join).await?;
        if rows.is_empty() {
            continue;
        }
        sidecars.push(ComplianceExportSidecarRows {
            table: table.clone(),
            rows,
        });
    }
    Ok(())
}

async fn sidecar_rows(
    pool: &PgPool,
    owner: OwnerRef,
    table: &str,
    join: SidecarJoin,
) -> Result<Vec<Value>, StorageError> {
    let table = PgIdent::table(table)?;
    let sidecar_column = PgIdent::column(join.sidecar_column)?;
    let base_table = PgIdent::table(join.base_table)?;
    let base_column = PgIdent::column(join.base_column)?;
    let (owner_kind, owner_id) = owner_binds(&owner);
    // SQL-POLICY: PgIdent
    let sql = format!(
        "SELECT to_jsonb(t)
           FROM {table} t
           JOIN {base_table} base
             ON base.{base_column} = t.{sidecar_column}
          WHERE base.owner_kind = $1
            AND base.owner_id IS NOT DISTINCT FROM $2
          ORDER BY t.{sidecar_column}",
        table = table.as_str(),
        base_table = base_table.as_str(),
        base_column = base_column.as_str(),
        sidecar_column = sidecar_column.as_str(),
    );
    // SQL-POLICY: PgIdent
    sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(sql))
        .bind(owner_kind)
        .bind(owner_id)
        .fetch_all(pool)
        .await
        .map_err(map_err)
}

const MEMORY_ROWS_SQL: &str = "
SELECT to_jsonb(m)
  FROM proxima_core.memories m
 WHERE m.owner_kind = $1
   AND m.owner_id IS NOT DISTINCT FROM $2
 ORDER BY m.created_at, m.memory_id";

const GOAL_ROWS_SQL: &str = "
SELECT to_jsonb(g)
  FROM proxima_core.goals g
 WHERE g.owner_kind = $1
   AND g.owner_id IS NOT DISTINCT FROM $2
 ORDER BY g.created_at, g.goal_id";

const EDGE_ROWS_SQL: &str = "
SELECT to_jsonb(e)
  FROM proxima_core.edges e
 WHERE e.owner_kind = $1
   AND e.owner_id IS NOT DISTINCT FROM $2
 ORDER BY e.created_at, e.source_kind, e.source_id, e.target_kind, e.target_id, e.kind";

const RECEIPT_ROWS_SQL: &str = "
SELECT to_jsonb(fr)
  FROM proxima_core.fact_receipts fr
 WHERE fr.owner_kind = $1
   AND fr.owner_id IS NOT DISTINCT FROM $2
 ORDER BY fr.observed_at, fr.receipt_id";

const SOURCE_BATCH_ROWS_SQL: &str = "
SELECT to_jsonb(sb)
  FROM proxima_core.source_batches sb
 WHERE sb.owner_kind = $1
   AND sb.owner_id IS NOT DISTINCT FROM $2
 ORDER BY sb.opened_at, sb.id";

const SOURCE_CURSOR_ROWS_SQL: &str = "
SELECT to_jsonb(sc)
  FROM proxima_core.source_cursors sc
 WHERE sc.owner_kind = $1
   AND sc.owner_id IS NOT DISTINCT FROM $2
 ORDER BY sc.source";

// Keep this an explicit field allowlist: the durable grant table is an
// unsupported persistence detail, while the compliance bundle is a supported
// serialized contract. A future storage-only column must not leak into export
// merely because the table changed.
const DELEGATED_AUTHORITY_GRANT_ROWS_SQL: &str = "
SELECT jsonb_build_object(
           'subject_user_id', dag.subject_user_id,
           'owner_kind', dag.owner_kind,
           'owner_id', dag.owner_id,
           'tool_name', dag.tool_name,
           'action_name', dag.action_name,
           'read_ceiling', dag.read_ceiling,
           'write_ceiling', dag.write_ceiling,
           'expires_at', dag.expires_at,
           'auth_epoch', dag.auth_epoch,
           'issued_at', dag.issued_at,
           'revoked_at', dag.revoked_at,
           'revoked_by_user_id', dag.revoked_by_user_id
       )
  FROM proxima_core.delegated_authority_grants dag
 WHERE dag.owner_kind = $1
   AND dag.owner_id IS NOT DISTINCT FROM $2
 ORDER BY dag.issued_at, dag.delegation_id";
