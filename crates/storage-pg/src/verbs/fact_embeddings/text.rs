use std::collections::HashMap;

use proxima_core::verbs::schema::MemorySearchProjection;
use proxima_core::{EntityKind, MemoryId, Owner, StorageError};
use sqlx::{Executor, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_ident::PgIdent;

/// Owner-scoped read of the rendered text stored on a Fact memory row.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub async fn load_fact_text(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
    projections: &[MemorySearchProjection],
) -> Result<Option<String>, StorageError> {
    load_embedding_text(pool, owner, EntityKind::Fact, memory_id, &[], projections).await
}

/// Owner-scoped embed text for one memory.
///
/// Text comes from the sidecar named by the row's schema in `projections`
/// (the frozen flavor search registry). A schema that is not projected,
/// or that is listed in `non_embeddable_schemas`, has nothing to embed.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub async fn load_embedding_text(
    pool: &PgPool,
    owner: &Owner,
    entity_kind: EntityKind,
    memory_id: MemoryId,
    non_embeddable_schemas: &[String],
    projections: &[MemorySearchProjection],
) -> Result<Option<String>, StorageError> {
    let texts = load_embedding_texts(
        pool,
        &[(*owner, entity_kind, memory_id)],
        non_embeddable_schemas,
        projections,
    )
    .await?;
    Ok(texts.into_iter().next().flatten())
}

/// Owner-scoped embed text for many memories, aligned with `items`.
///
/// One `memory` lookup for the id set, then one sidecar `embed_text`
/// SELECT per distinct `(table, column)` in that set.
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub async fn load_embedding_texts(
    pool: &PgPool,
    items: &[(Owner, EntityKind, MemoryId)],
    non_embeddable_schemas: &[String],
    projections: &[MemorySearchProjection],
) -> Result<Vec<Option<String>>, StorageError> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = items
        .iter()
        .map(|(_, _, memory_id)| memory_id.into_inner())
        .collect();
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT t, owner_id, schema_id
           FROM proxima_core.memory
          WHERE t = ANY($1::uuid[])",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let mut schema_by_key = HashMap::with_capacity(rows.len());
    for (t, owner_id, schema_id) in rows {
        schema_by_key.insert((t, owner_id), schema_id);
    }

    let mut out = vec![None; items.len()];
    let mut buckets: HashMap<(String, String), Vec<(usize, Uuid)>> = HashMap::new();
    for (index, (owner, _, memory_id)) in items.iter().enumerate() {
        let t = memory_id.into_inner();
        let Some(schema_id) = schema_by_key.get(&(t, owner.stored_owner_id())) else {
            continue;
        };
        let Some(projection) = resolve_projection(
            Some(schema_id.as_str()),
            non_embeddable_schemas,
            projections,
        ) else {
            continue;
        };
        let Some(column) = projection.embed_text_column.as_deref() else {
            continue;
        };
        buckets
            .entry((projection.sidecar_table.clone(), column.to_owned()))
            .or_default()
            .push((index, t));
    }

    for ((table, column), members) in buckets {
        let table = PgIdent::table(&table)?;
        let column = PgIdent::column(&column)?;
        let member_ids: Vec<Uuid> = members.iter().map(|(_, t)| *t).collect();
        let sql = format!(
            "SELECT c.t, c.{column}
               FROM {table} c
              WHERE c.t = ANY($1::uuid[])",
            table = table.as_str(),
            column = column.as_str(),
        );
        // SQL-POLICY: PgIdent
        let texts: Vec<(Uuid, Option<String>)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(&member_ids)
            .fetch_all(pool)
            .await
            .map_err(map_err)?;
        let mut text_by_t = HashMap::with_capacity(texts.len());
        for (t, text) in texts {
            if let Some(text) = text {
                text_by_t.insert(t, text);
            }
        }
        for (index, t) in members {
            if let Some(text) = text_by_t.get(&t) {
                out[index] = Some(text.clone());
            }
        }
    }
    Ok(out)
}

/// Transaction-scoped variant of [`load_fact_text`].
///
/// # Errors
///
/// Returns `StorageError::Internal` for SQL failures.
pub async fn load_fact_text_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    memory_id: MemoryId,
    projections: &[MemorySearchProjection],
) -> Result<Option<String>, StorageError> {
    let schema_id = fetch_schema_id(tx.as_mut(), owner, memory_id).await?;
    let Some(projection) = resolve_projection(schema_id.as_deref(), &[], projections) else {
        return Ok(None);
    };
    fetch_projection_text(tx.as_mut(), memory_id, projection).await
}

fn resolve_projection<'a>(
    schema_id: Option<&str>,
    non_embeddable_schemas: &[String],
    projections: &'a [MemorySearchProjection],
) -> Option<&'a MemorySearchProjection> {
    let schema_id = schema_id?;
    if non_embeddable_schemas
        .iter()
        .any(|excluded| excluded == schema_id)
    {
        return None;
    }
    projections
        .iter()
        .find(|projection| projection.schema_id.as_str() == schema_id)
}

async fn fetch_schema_id<'e, E>(
    exec: E,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<Option<String>, StorageError>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_scalar(
        "SELECT m.schema_id
           FROM proxima_core.memory m
          WHERE m.t = $1
            AND m.owner_id = $2",
    )
    .bind(memory_id.into_inner())
    .bind(owner.stored_owner_id())
    .fetch_optional(exec)
    .await
    .map_err(map_err)
}

async fn fetch_projection_text<'e, E>(
    exec: E,
    memory_id: MemoryId,
    projection: &MemorySearchProjection,
) -> Result<Option<String>, StorageError>
where
    E: Executor<'e, Database = Postgres>,
{
    let Some(column) = projection.embed_text_column.as_deref() else {
        return Ok(None);
    };
    let table = PgIdent::table(&projection.sidecar_table)?;
    let column = PgIdent::column(column)?;
    let sql = format!(
        "SELECT c.{column}
           FROM {table} c
          WHERE c.t = $1",
        table = table.as_str(),
        column = column.as_str(),
    );
    // SQL-POLICY: PgIdent
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(memory_id.into_inner())
        .fetch_optional(exec)
        .await
        .map_err(map_err)
}

#[cfg(test)]
mod tests {
    #[test]
    fn embed_text_schema_is_on_memory() {
        let src = include_str!("text.rs");
        let join = format!(
            "{}{}",
            "JOIN proxima_core.memory_head h ON h.handle", " = m.handle"
        );
        assert!(
            !src.contains(&join),
            "W5: embed text reads schema_id from memory"
        );
    }

    #[test]
    fn drain_reads_stored_embed_text() {
        let src = include_str!("text.rs");
        let concat = format!("{}{}", "concat_ws", "(' ',");
        assert!(
            !src.contains(&concat),
            "W6: drain does not re-concat projection columns"
        );
        assert!(
            src.contains("embed_text_column"),
            "W6: drain selects the stored sidecar column"
        );
    }
}
