//! Shared hard-delete row fan-out.

use std::collections::HashSet;

use proxima_core::{EntityKind, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_ident::PgIdent;

#[derive(Debug)]
pub struct HardDeleteSet {
    /// `(entity_kind, memory_id)` — embeddings are deleted by BOTH columns
    /// to avoid cross-kind UUID collisions in the polymorphic embeddings PK.
    pub memories: Vec<(EntityKind, Uuid)>,
    /// Edge rows to delete. Edge-keyed sidecars are deleted first.
    pub edge_ids: Vec<Uuid>,
    /// Event rows to delete (bytea event ids), deleted after memories.
    pub event_ids: Vec<Vec<u8>>,
}

#[derive(Debug)]
pub struct HardDeleteSidecars<'a> {
    pub memory_keyed: &'a [String],
    pub edge_keyed: &'a [String],
    pub citation_mapping_keyed: &'a [String],
}

#[derive(Debug, Default, Clone, Copy)]
pub struct HardDeleteCounts {
    pub edges: u64,
    pub embeddings: u64,
    pub citation_mappings: u64,
    pub memories: u64,
    pub events: u64,
}

/// Fan-out hard deletion of memory/edge/event rows and their registered
/// sidecars, returning per-table deleted counts.
///
/// # Errors
///
/// Returns storage errors from any of the underlying row deletions.
pub async fn execute_hard_delete(
    tx: &mut Transaction<'_, Postgres>,
    set: &HardDeleteSet,
    sidecars: &HardDeleteSidecars<'_>,
) -> Result<HardDeleteCounts, StorageError> {
    let mut counts = HardDeleteCounts::default();
    let memory_ids = set
        .memories
        .iter()
        .map(|(_, memory_id)| *memory_id)
        .collect::<Vec<_>>();

    delete_edge_keyed_sidecars(tx, sidecars.edge_keyed, &set.edge_ids).await?;
    counts.edges = delete_edges(tx, &set.edge_ids).await?;

    delete_memory_keyed_sidecars(tx, sidecars.memory_keyed, &memory_ids).await?;
    counts.embeddings = delete_embeddings(tx, &set.memories).await?;
    delete_embedding_jobs(tx, &memory_ids).await?;

    let citation_mapping_ids = citation_mapping_ids(tx, &memory_ids).await?;
    delete_citation_mapping_keyed_sidecars(
        tx,
        sidecars.citation_mapping_keyed,
        &citation_mapping_ids,
    )
    .await?;

    counts.citation_mappings = delete_citation_mappings(tx, &memory_ids).await?;
    counts.memories = delete_memories(tx, &memory_ids).await?;
    counts.events = delete_events(tx, &set.event_ids).await?;

    Ok(counts)
}

async fn delete_embedding_jobs(
    tx: &mut Transaction<'_, Postgres>,
    memory_ids: &[Uuid],
) -> Result<(), StorageError> {
    if memory_ids.is_empty() {
        return Ok(());
    }

    sqlx::query("DELETE FROM proxima_core.embedding_jobs WHERE entity_id = ANY($1::uuid[])")
        .bind(memory_ids)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}

async fn delete_edge_keyed_sidecars(
    tx: &mut Transaction<'_, Postgres>,
    tables: &[String],
    edge_ids: &[Uuid],
) -> Result<(), StorageError> {
    if edge_ids.is_empty() {
        return Ok(());
    }

    for table in tables {
        let table = PgIdent::table(table)?;
        let sql = format!(
            "DELETE FROM {table} WHERE edge_id = ANY($1::uuid[])",
            table = table.as_str(),
        );
        sqlx::query(&sql)
            .bind(edge_ids)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

async fn delete_edges(
    tx: &mut Transaction<'_, Postgres>,
    edge_ids: &[Uuid],
) -> Result<u64, StorageError> {
    if edge_ids.is_empty() {
        return Ok(0);
    }

    let deleted = sqlx::query("DELETE FROM proxima_core.edges WHERE edge_id = ANY($1::uuid[])")
        .bind(edge_ids)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(deleted.rows_affected())
}

async fn delete_memory_keyed_sidecars(
    tx: &mut Transaction<'_, Postgres>,
    tables: &[String],
    memory_ids: &[Uuid],
) -> Result<(), StorageError> {
    if memory_ids.is_empty() {
        return Ok(());
    }

    for table in tables {
        let table = PgIdent::table(table)?;
        let sql = format!(
            "DELETE FROM {table} WHERE memory_id = ANY($1::uuid[])",
            table = table.as_str(),
        );
        sqlx::query(&sql)
            .bind(memory_ids)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

async fn delete_embeddings(
    tx: &mut Transaction<'_, Postgres>,
    memories: &[(EntityKind, Uuid)],
) -> Result<u64, StorageError> {
    if memories.is_empty() {
        return Ok(0);
    }

    let kinds = memories
        .iter()
        .map(|(kind, _)| *kind)
        .collect::<HashSet<_>>();
    let mut count = 0;
    for kind in [
        EntityKind::Fact,
        EntityKind::Abstraction,
        EntityKind::Perspective,
        EntityKind::Goal,
    ] {
        if !kinds.contains(&kind) {
            continue;
        }

        let ids = memories
            .iter()
            .filter_map(|(candidate_kind, memory_id)| {
                (*candidate_kind == kind).then_some(*memory_id)
            })
            .collect::<Vec<_>>();
        if ids.is_empty() {
            continue;
        }

        let deleted = sqlx::query(
            "DELETE FROM proxima_core.embeddings
              WHERE entity_kind = $1
                AND entity_id = ANY($2::uuid[])",
        )
        .bind(kind)
        .bind(&ids)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
        count += deleted.rows_affected();
    }
    Ok(count)
}

async fn citation_mapping_ids(
    tx: &mut Transaction<'_, Postgres>,
    memory_ids: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if memory_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_scalar(
        "SELECT citation_mapping_id
           FROM proxima_core.memories
          WHERE memory_id = ANY($1::uuid[])
            AND citation_mapping_id IS NOT NULL",
    )
    .bind(memory_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

async fn delete_citation_mapping_keyed_sidecars(
    tx: &mut Transaction<'_, Postgres>,
    tables: &[String],
    citation_mapping_ids: &[Uuid],
) -> Result<(), StorageError> {
    if citation_mapping_ids.is_empty() {
        return Ok(());
    }

    for table in tables {
        let table = PgIdent::table(table)?;
        let sql = format!(
            "DELETE FROM {table} WHERE citation_mapping_id = ANY($1::uuid[])",
            table = table.as_str(),
        );
        sqlx::query(&sql)
            .bind(citation_mapping_ids)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

async fn delete_citation_mappings(
    tx: &mut Transaction<'_, Postgres>,
    memory_ids: &[Uuid],
) -> Result<u64, StorageError> {
    if memory_ids.is_empty() {
        return Ok(0);
    }

    let deleted = sqlx::query(
        "DELETE FROM proxima_core.citation_mappings
          WHERE memory_id = ANY($1::uuid[])",
    )
    .bind(memory_ids)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(deleted.rows_affected())
}

async fn delete_memories(
    tx: &mut Transaction<'_, Postgres>,
    memory_ids: &[Uuid],
) -> Result<u64, StorageError> {
    if memory_ids.is_empty() {
        return Ok(0);
    }

    let deleted =
        sqlx::query("DELETE FROM proxima_core.memories WHERE memory_id = ANY($1::uuid[])")
            .bind(memory_ids)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    Ok(deleted.rows_affected())
}

async fn delete_events(
    tx: &mut Transaction<'_, Postgres>,
    event_ids: &[Vec<u8>],
) -> Result<u64, StorageError> {
    if event_ids.is_empty() {
        return Ok(0);
    }

    let deleted = sqlx::query("DELETE FROM proxima_core.events WHERE event_id = ANY($1::bytea[])")
        .bind(event_ids)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(deleted.rows_affected())
}
