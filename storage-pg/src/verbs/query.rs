//! `Query` verb — paginated read of `memories` with optional
//! head filtering. Two head modes (docs/02 §Re-derivation, docs/03
//! §Stateful Fact schemas):
//!
//! - A/P: `NOT EXISTS (m2.supersedes = m.memory_id)` (lineage scan).
//! - Stateful Fact: `NOT EXISTS` of a row under the same NK tuple
//!   with a later `created_at` (head-by-natural-key).
//!
//! `stateful_heads` is set by the engine from the schema registry
//! before dispatch when the request is heads-only and `schema_id`
//! resolves to a stateful Fact schema.

use std::fmt::Write as _;

use proxima_core::verbs::query::{
    EntityKind, MemoryRow, QueryRequest, QueryResponse, StatefulHeadsFilter, SupersessionStatus,
};
use proxima_core::{
    GroupId, MemoryId, OrgId, Owner, Principal, SchemaId, SchemaVersion, StorageError, UserId,
};
use sqlx::PgPool;

#[allow(clippy::too_many_lines)]
pub(crate) async fn query_memories(
    pool: &PgPool,
    req: &QueryRequest,
) -> Result<QueryResponse, StorageError> {
    let owner_kind: &str = match &req.owner.principal {
        Principal::User(_) => "User",
        Principal::Group(_) => "Group",
    };
    let owner_principal_id = match &req.owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    let owner_org_id = req.owner.org_id.into_inner();

    if matches!(req.entity_kind, Some(EntityKind::Goal)) {
        return Ok(QueryResponse {
            memories: Vec::new(),
            seq_high_water: read_seq_high_water(
                pool,
                owner_kind,
                owner_principal_id,
                owner_org_id,
            )
            .await?,
        });
    }

    let stateful = req
        .stateful_heads
        .as_ref()
        .filter(|_| matches!(req.supersession, SupersessionStatus::HeadsOnly))
        .map(validate_stateful_filter)
        .transpose()?;

    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());

    let mut sql = String::from(
        "SELECT m.memory_id, m.owner_principal_kind, m.owner_principal_id, \
                m.owner_org_id, m.schema_id, m.kind, m.event_id \
         FROM proxima_core.memories m",
    );

    if let Some(sf) = &stateful {
        write!(sql, " JOIN {sidecar} s USING (memory_id)", sidecar = sf.sidecar_table)
            .expect("write to String is infallible");
    }

    sql.push_str(
        " WHERE m.owner_principal_kind = $1 AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3",
    );

    match req.entity_kind {
        None => {}
        Some(EntityKind::Fact) => {
            sql.push_str(" AND m.event_id IS NOT NULL AND m.kind IS NULL");
        }
        Some(EntityKind::Abstraction) => sql.push_str(" AND m.kind = 'Abstraction'"),
        Some(EntityKind::Perspective) => sql.push_str(" AND m.kind = 'Perspective'"),
        Some(EntityKind::Goal) => unreachable!(),
    }

    if schema_id_filter.is_some() {
        sql.push_str(" AND m.schema_id = $4");
    }

    if matches!(req.supersession, SupersessionStatus::HeadsOnly) {
        if let Some(sf) = &stateful {
            // Head-by-natural-key: latest memories.created_at per NK tuple
            // under the same schema_id. docs/03 §Stateful Fact schemas.
            let nk_pairs = sf
                .natural_key_columns
                .iter()
                .map(|c| format!("s2.{c} = s.{c}"))
                .collect::<Vec<_>>()
                .join(" AND ");
            write!(
                sql,
                " AND NOT EXISTS ( \
                     SELECT 1 FROM proxima_core.memories m2 \
                     JOIN {sidecar} s2 USING (memory_id) \
                     WHERE m2.schema_id = m.schema_id \
                       AND m2.owner_principal_kind = m.owner_principal_kind \
                       AND m2.owner_principal_id = m.owner_principal_id \
                       AND m2.owner_org_id = m.owner_org_id \
                       AND {nk_pairs} \
                       AND m2.created_at > m.created_at \
                  )",
                sidecar = sf.sidecar_table,
                nk_pairs = nk_pairs
            )
            .expect("write to String is infallible");
        } else {
            sql.push_str(
                " AND NOT EXISTS (SELECT 1 FROM proxima_core.memories m2 \
                                  WHERE m2.supersedes = m.memory_id)",
            );
        }
    }

    sql.push_str(" ORDER BY m.created_at DESC LIMIT ");
    sql.push_str(&u64::from(req.limit).to_string());

    let mut q = sqlx::query_as::<_, MemoryRowDb>(&sql)
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner_org_id);
    if let Some(sid) = &schema_id_filter {
        q = q.bind(sid.clone());
    }

    let rows: Vec<MemoryRowDb> = q
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    let memories: Vec<MemoryRow> = rows
        .into_iter()
        .map(|r| MemoryRow {
            id: MemoryId::new(r.memory_id),
            kind: match r.kind.as_deref() {
                Some("Abstraction") => EntityKind::Abstraction,
                Some("Perspective") => EntityKind::Perspective,
                _ => EntityKind::Fact,
            },
            schema_id: SchemaId::new(r.schema_id),
            schema_version: SchemaVersion::new(1),
            owner: Owner {
                principal: match r.owner_principal_kind.as_str() {
                    "User" => Principal::User(UserId::new(r.owner_principal_id)),
                    _ => Principal::Group(GroupId::new(r.owner_principal_id)),
                },
                org_id: OrgId::new(r.owner_org_id),
            },
        })
        .collect();

    let seq_high_water =
        read_seq_high_water(pool, owner_kind, owner_principal_id, owner_org_id).await?;

    Ok(QueryResponse {
        memories,
        seq_high_water,
    })
}

#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
struct MemoryRowDb {
    memory_id: uuid::Uuid,
    owner_principal_kind: String,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
    schema_id: String,
    kind: Option<String>,
    event_id: Option<Vec<u8>>,
}

async fn read_seq_high_water(
    pool: &PgPool,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
) -> Result<Option<uuid::Uuid>, StorageError> {
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT seq FROM proxima_core.change_event \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 \
           AND owner_org_id = $3 \
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| StorageError::Internal(e.to_string()))?;
    Ok(row.map(|(v,)| v))
}

/// Validate identifiers from `StatefulHeadsFilter` before splicing them
/// into SQL. The values come from build-time-registered schemas
/// (`FactPayload::sidecar_table`, `FactPayload::natural_key_columns`)
/// which are `&'static str` constants — author-controlled, not
/// caller-controlled. This is a defense-in-depth check that catches
/// typos and rejects anything that doesn't look like a postgres
/// identifier.
fn validate_stateful_filter(
    sf: &StatefulHeadsFilter,
) -> Result<&StatefulHeadsFilter, StorageError> {
    if !is_qualified_table_ident(&sf.sidecar_table) {
        return Err(StorageError::Internal(format!(
            "invalid sidecar_table identifier: {:?}",
            sf.sidecar_table
        )));
    }
    if sf.natural_key_columns.is_empty() {
        return Err(StorageError::Internal(
            "stateful_heads with empty natural_key_columns".into(),
        ));
    }
    for col in &sf.natural_key_columns {
        if !is_column_ident(col) {
            return Err(StorageError::Internal(format!(
                "invalid natural_key column identifier: {col:?}"
            )));
        }
    }
    Ok(sf)
}

fn is_qualified_table_ident(s: &str) -> bool {
    // Allow `schema.table` (single dot) or `table`.
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() > 2 || parts.is_empty() {
        return false;
    }
    parts.iter().all(|p| is_column_ident(p))
}

fn is_column_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s.len() <= 63
}
