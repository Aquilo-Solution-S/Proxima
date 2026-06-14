//! Owner Fact-retention sweep.

use proxima_core::verbs::fact_cleanup::CleanupDueFactsOutcome;
use proxima_core::{EntityKind, Owner, OwnerPrincipalKind, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::consolidate::owner_columns;
use crate::verbs::fact_retention::get_fact_retention_in_tx;

#[derive(Debug, sqlx::FromRow)]
struct DueFactRow {
    memory_id: Uuid,
    event_id: Vec<u8>,
    schema_id: String,
    schema_version: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct TombstonedDerivativeRow {
    memory_id: Uuid,
    kind: EntityKind,
    schema_id: String,
    schema_version: i32,
}

#[derive(Debug, Clone, Copy)]
struct OwnerColumns {
    kind: OwnerPrincipalKind,
    principal_id: Uuid,
    org_id: Uuid,
}

#[derive(Debug, Clone, Copy)]
struct DeleteEntityEvent<'a> {
    kind: EntityKind,
    memory_id: Uuid,
    schema_id: &'a str,
    schema_version: i32,
}

/// Hard-erase due Facts and tombstone their direct Provenance dependents.
///
/// # Errors
///
/// Returns storage constraint/internal errors from Postgres.
pub async fn cleanup_due_facts(
    pool: &sqlx::PgPool,
    owner: &Owner,
    fact_sidecar_tables: &[String],
    citation_mapping_sidecar_tables: &[String],
) -> Result<CleanupDueFactsOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    let outcome = cleanup_due_facts_in_tx(
        &mut tx,
        owner,
        fact_sidecar_tables,
        citation_mapping_sidecar_tables,
    )
    .await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

async fn cleanup_due_facts_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    fact_sidecar_tables: &[String],
    citation_mapping_sidecar_tables: &[String],
) -> Result<CleanupDueFactsOutcome, StorageError> {
    let Some(retention_seconds) = get_fact_retention_in_tx(tx, owner).await? else {
        return Ok(CleanupDueFactsOutcome {
            facts_erased: 0,
            derivatives_tombstoned: 0,
        });
    };

    let (owner_kind, owner_principal_id, owner_org_id) = owner_columns(owner);
    let owner_columns = OwnerColumns {
        kind: owner_kind,
        principal_id: owner_principal_id,
        org_id: owner_org_id,
    };
    let due: Vec<DueFactRow> = sqlx::query_as(
        "SELECT memory_id, event_id, schema_id, schema_version
           FROM proxima_core.memories
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND event_id IS NOT NULL
            AND citation_mapping_id IS NOT NULL
            AND tombstoned_at IS NULL
            AND created_at < now() - ($4::double precision * INTERVAL '1 second')
          ORDER BY created_at ASC, memory_id ASC",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(retention_seconds)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;

    if due.is_empty() {
        return Ok(CleanupDueFactsOutcome {
            facts_erased: 0,
            derivatives_tombstoned: 0,
        });
    }

    let due_memory_ids = due.iter().map(|row| row.memory_id).collect::<Vec<_>>();
    let tombstoned = tombstone_direct_derivatives(
        tx,
        owner_kind,
        owner_principal_id,
        owner_org_id,
        &due_memory_ids,
    )
    .await?;

    for row in &tombstoned {
        insert_entity_delete_event(
            tx,
            owner_columns,
            DeleteEntityEvent {
                kind: row.kind,
                memory_id: row.memory_id,
                schema_id: &row.schema_id,
                schema_version: row.schema_version,
            },
        )
        .await?;
    }
    for row in &due {
        insert_entity_delete_event(
            tx,
            owner_columns,
            DeleteEntityEvent {
                kind: EntityKind::Fact,
                memory_id: row.memory_id,
                schema_id: &row.schema_id,
                schema_version: row.schema_version,
            },
        )
        .await?;
    }

    delete_edges_referencing_facts(
        tx,
        owner_kind,
        owner_principal_id,
        owner_org_id,
        &due_memory_ids,
    )
    .await?;
    delete_fact_sidecars(tx, fact_sidecar_tables, &due_memory_ids).await?;
    delete_citation_mapping_sidecars(tx, citation_mapping_sidecar_tables, &due_memory_ids).await?;
    delete_fact_core_rows(tx, &due).await?;

    Ok(CleanupDueFactsOutcome {
        facts_erased: u64::try_from(due.len()).unwrap_or(u64::MAX),
        derivatives_tombstoned: u64::try_from(tombstoned.len()).unwrap_or(u64::MAX),
    })
}

async fn tombstone_direct_derivatives(
    tx: &mut Transaction<'_, Postgres>,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    owner_org_id: Uuid,
    due_memory_ids: &[Uuid],
) -> Result<Vec<TombstonedDerivativeRow>, StorageError> {
    sqlx::query_as(
        "UPDATE proxima_core.memories m
            SET tombstoned_at = now()
           FROM proxima_core.edges e
          WHERE e.owner_principal_kind = $1
            AND e.owner_principal_id = $2
            AND e.owner_org_id = $3
            AND e.relation_class = 'Provenance'
            AND e.target_memory_id = ANY($4::uuid[])
            AND e.source_memory_id = m.memory_id
            AND m.owner_principal_kind = e.owner_principal_kind
            AND m.owner_principal_id = e.owner_principal_id
            AND m.owner_org_id = e.owner_org_id
            AND m.kind IS NOT NULL
            AND m.tombstoned_at IS NULL
          RETURNING m.memory_id, m.kind, m.schema_id, m.schema_version",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(due_memory_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

async fn insert_entity_delete_event(
    tx: &mut Transaction<'_, Postgres>,
    owner: OwnerColumns,
    entity: DeleteEntityEvent<'_>,
) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO proxima_core.change_event
            (seq, owner_principal_kind, owner_principal_id, owner_org_id, kind,
             entity_kind, entity_memory_id, entity_schema_id, entity_schema_version)
         VALUES ($1, $2, $3, $4, 'EntityDelete', $5, $6, $7, $8)",
    )
    .bind(Uuid::now_v7())
    .bind(owner.kind)
    .bind(owner.principal_id)
    .bind(owner.org_id)
    .bind(entity.kind)
    .bind(entity.memory_id)
    .bind(entity.schema_id)
    .bind(entity.schema_version)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn delete_edges_referencing_facts(
    tx: &mut Transaction<'_, Postgres>,
    owner_kind: OwnerPrincipalKind,
    owner_principal_id: Uuid,
    owner_org_id: Uuid,
    due_memory_ids: &[Uuid],
) -> Result<(), StorageError> {
    sqlx::query(
        "DELETE FROM proxima_core.edges
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND (
                source_memory_id = ANY($4::uuid[])
                OR target_memory_id = ANY($4::uuid[])
                OR authorship_owner_memory_id = ANY($4::uuid[])
            )",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(due_memory_ids)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn delete_fact_sidecars(
    tx: &mut Transaction<'_, Postgres>,
    fact_sidecar_tables: &[String],
    due_memory_ids: &[Uuid],
) -> Result<(), StorageError> {
    for table in fact_sidecar_tables {
        let table = PgIdent::table(table)?;
        let sql = format!(
            "DELETE FROM {table} WHERE memory_id = ANY($1::uuid[])",
            table = table.as_str(),
        );
        sqlx::query(&sql)
            .bind(due_memory_ids)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

async fn delete_citation_mapping_sidecars(
    tx: &mut Transaction<'_, Postgres>,
    citation_mapping_sidecar_tables: &[String],
    due_memory_ids: &[Uuid],
) -> Result<(), StorageError> {
    let citation_mapping_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT citation_mapping_id
           FROM proxima_core.memories
          WHERE memory_id = ANY($1::uuid[])
            AND citation_mapping_id IS NOT NULL",
    )
    .bind(due_memory_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;

    for table in citation_mapping_sidecar_tables {
        let table = PgIdent::table(table)?;
        let sql = format!(
            "DELETE FROM {table} WHERE citation_mapping_id = ANY($1::uuid[])",
            table = table.as_str(),
        );
        sqlx::query(&sql)
            .bind(&citation_mapping_ids)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

async fn delete_fact_core_rows(
    tx: &mut Transaction<'_, Postgres>,
    due: &[DueFactRow],
) -> Result<(), StorageError> {
    let due_memory_ids = due.iter().map(|row| row.memory_id).collect::<Vec<_>>();
    sqlx::query(
        "DELETE FROM proxima_core.embeddings
          WHERE entity_kind = 'Fact'
            AND entity_id = ANY($1::uuid[])",
    )
    .bind(&due_memory_ids)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "DELETE FROM proxima_core.citation_mappings
          WHERE memory_id = ANY($1::uuid[])",
    )
    .bind(&due_memory_ids)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.memories WHERE memory_id = ANY($1::uuid[])")
        .bind(&due_memory_ids)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    for row in due {
        sqlx::query("DELETE FROM proxima_core.events WHERE event_id = $1")
            .bind(&row.event_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}
