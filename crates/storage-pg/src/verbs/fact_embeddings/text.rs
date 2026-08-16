use proxima_core::verbs::schema::{MemorySearchProjection, MemorySearchProjectionField};
use proxima_core::{EntityKind, MemoryId, Owner, SearchProjectionColumnKind, StorageError};
use sqlx::{Executor, PgPool, Postgres, Transaction};

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
    load_embedding_text_on(
        pool,
        pool,
        owner,
        entity_kind,
        memory_id,
        non_embeddable_schemas,
        projections,
    )
    .await
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

async fn load_embedding_text_on<'s, 't, S, T>(
    schema_exec: S,
    text_exec: T,
    owner: &Owner,
    _entity_kind: EntityKind,
    memory_id: MemoryId,
    non_embeddable_schemas: &[String],
    projections: &[MemorySearchProjection],
) -> Result<Option<String>, StorageError>
where
    S: Executor<'s, Database = Postgres>,
    T: Executor<'t, Database = Postgres>,
{
    let schema_id = fetch_schema_id(schema_exec, owner, memory_id).await?;
    let Some(projection) =
        resolve_projection(schema_id.as_deref(), non_embeddable_schemas, projections)
    else {
        return Ok(None);
    };
    fetch_projection_text(text_exec, memory_id, projection).await
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
        "SELECT h.schema_id
           FROM proxima_core.memory m
           JOIN proxima_core.memory_head h ON h.handle = m.handle
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
    let Some(text_expr) = projection_embed_text(&projection.fields)? else {
        return Ok(None);
    };
    let table = PgIdent::table(&projection.sidecar_table)?;
    let sql = format!(
        "SELECT {text_expr}
           FROM {table} c
          WHERE c.t = $1",
        table = table.as_str(),
    );
    // SQL-POLICY: PgIdent
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(memory_id.into_inner())
        .fetch_optional(exec)
        .await
        .map_err(map_err)
}

fn projection_embed_text(
    fields: &[MemorySearchProjectionField],
) -> Result<Option<String>, StorageError> {
    let mut expressions = Vec::new();
    for field in fields {
        if matches!(field.kind, SearchProjectionColumnKind::MemoryText) {
            continue;
        }
        let column = PgIdent::column(&field.column)?;
        let expression = match field.kind {
            SearchProjectionColumnKind::Text => {
                format!("NULLIF(c.{}::text, '')", column.as_str())
            }
            SearchProjectionColumnKind::TextArray => {
                format!("NULLIF(array_to_string(c.{}, ' '), '')", column.as_str())
            }
            SearchProjectionColumnKind::MemoryText => unreachable!("skipped above"),
        };
        expressions.push(expression);
    }
    if expressions.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!(
        "NULLIF(concat_ws(' ', {}), '')",
        expressions.join(", ")
    )))
}
