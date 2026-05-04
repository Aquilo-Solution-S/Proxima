//! `Query` verb — paginated read of `memories` with optional
//! supersession-head filtering.

use proxima_core::verbs::query::{
    EntityKind, MemoryRow, QueryRequest, QueryResponse, SupersessionStatus,
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

    let mut sql = String::from(
        "SELECT memory_id, owner_principal_kind, owner_principal_id, \
                owner_org_id, schema_id, kind, event_id \
         FROM proxima_core.memories \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 \
           AND owner_org_id = $3",
    );
    match req.entity_kind {
        None => {}
        Some(EntityKind::Fact) => {
            sql.push_str(" AND event_id IS NOT NULL AND kind IS NULL");
        }
        Some(EntityKind::Abstraction) => sql.push_str(" AND kind = 'Abstraction'"),
        Some(EntityKind::Perspective) => sql.push_str(" AND kind = 'Perspective'"),
        Some(EntityKind::Goal) => unreachable!(),
    }

    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());
    if schema_id_filter.is_some() {
        sql.push_str(" AND schema_id = $4");
    }

    if matches!(req.supersession, SupersessionStatus::HeadsOnly) {
        sql.push_str(
            " AND NOT EXISTS (SELECT 1 FROM proxima_core.memories m2 \
                              WHERE m2.supersedes = proxima_core.memories.memory_id)",
        );
    }

    sql.push_str(" ORDER BY created_at DESC LIMIT ");
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
    // Postgres has no MAX(uuid). UUIDv7 sorts lexicographically by
    // generation time, so ORDER BY seq DESC LIMIT 1 is the
    // monotonic high-water for our usage.
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
