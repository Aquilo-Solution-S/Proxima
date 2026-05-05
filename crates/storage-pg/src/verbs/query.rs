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
//!
//! Payload projection: for each schema with a sidecar table, we
//! LEFT JOIN the sidecar and project `row_to_json(s_*)::text` via
//! a CASE expression. The result is mapped to `MemoryRow.payload`.

use std::fmt::Write as _;

use proxima_core::verbs::query::{
    EntityKind, MemoryRow, QueryRequest, QueryResponse, StatefulHeadsFilter, SupersessionStatus,
};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{
    GroupId, MemoryId, OrgId, Owner, Principal, SchemaId, SchemaVersion, StorageError, UserId,
};
use sqlx::PgPool;

use crate::pg_ident::PgIdent;

#[allow(clippy::too_many_lines)]
pub(crate) async fn query_memories(
    pool: &PgPool,
    req: &QueryRequest,
    schemas: &[SchemaInfo],
) -> Result<QueryResponse, StorageError> {
    let owner_kind: &str = match &req.owner.principal {
        Principal::User(_) => "User",
        Principal::Group(_) => "Group",
    };
    let owner_principal_id = match &req.owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    if matches!(req.entity_kind, Some(EntityKind::Goal)) {
        return Ok(QueryResponse {
            memories: Vec::new(),
            seq_high_water: read_seq_high_water(pool, owner_kind, owner_principal_id).await?,
        });
    }

    let stateful = req
        .stateful_heads
        .as_ref()
        .filter(|_| matches!(req.supersession, SupersessionStatus::HeadsOnly))
        .map(validate_stateful_filter)
        .transpose()?;

    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());

    // Build payload projection: for each F/A/P schema with a sidecar
    // table, LEFT JOIN the sidecar and emit a CASE expression that
    // picks the matching `row_to_json(s_*)::text`.
    //
    // Edge sidecars are keyed on `edge_id`, not `memory_id` — they
    // don't participate in memory queries. Goal sidecars don't either:
    // Goals are a distinct entity (AGENTS.md invariant 11), and goal
    // queries short-circuit above.
    let schemas_with_sidecar: Vec<&SchemaInfo> = schemas
        .iter()
        .filter(|s| {
            s.sidecar_table.is_some()
                && matches!(
                    s.kind,
                    PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
                )
        })
        .collect();

    let mut sql = String::from(
        "SELECT m.memory_id, m.owner_principal_kind, m.owner_principal_id, \
                m.owner_org_id, m.schema_id, m.schema_version, m.kind, m.event_id,",
    );

    // Bindings: $1=owner_kind, $2=owner_principal_id. $3 is reserved
    // for `schema_id_filter` when set. Schema-id literals
    // for the payload-projection CASE always come AFTER the optional
    // filter binding, so their parameter index depends on whether the
    // filter is present.
    let case_param_base = if schema_id_filter.is_some() { 4 } else { 3 };

    // If there are schemas with sidecars, add the payload_json CASE expression.
    // Otherwise, just add NULL as payload_json.
    if schemas_with_sidecar.is_empty() {
        sql.push_str(" NULL AS payload_json");
    } else {
        sql.push_str(" CASE");
        for (idx, schema) in schemas_with_sidecar.iter().enumerate() {
            let sidecar_table = schema.sidecar_table.as_ref().unwrap();
            PgIdent::table(sidecar_table)?;
            let alias = format!("s_{idx}");
            write!(
                sql,
                " WHEN m.schema_id = ${} AND m.schema_version = ${} \
                  THEN row_to_json({alias})::text",
                case_param_base + (idx * 2),
                case_param_base + (idx * 2) + 1,
                alias = alias
            )
            .expect("write to String is infallible");
        }
        sql.push_str(" ELSE NULL END AS payload_json");
    }

    sql.push_str(
        " \
         FROM proxima_core.memories m",
    );

    // Add LEFT JOINs for each schema with a sidecar table
    for (idx, schema) in schemas_with_sidecar.iter().enumerate() {
        let sidecar_table = PgIdent::table(schema.sidecar_table.as_ref().unwrap())?;
        let alias = format!("s_{idx}");
        write!(
            sql,
            " LEFT JOIN {sidecar_table} {alias} ON {alias}.memory_id = m.memory_id",
            sidecar_table = sidecar_table.as_str(),
        )
        .expect("write to String is infallible");
    }

    // The stateful JOIN (for head-by-natural-key filtering) uses a
    // separate alias to avoid colliding with the payload JOINs. We
    // use explicit `ON sf.memory_id = m.memory_id` rather than
    // `USING (memory_id)` because the payload LEFT JOINs above each
    // contribute their own `memory_id` to the left side, and
    // `USING` would barf with "common column name appears more than
    // once".
    if let Some(sf) = &stateful {
        write!(
            sql,
            " LEFT JOIN {sidecar} sf ON sf.memory_id = m.memory_id",
            sidecar = sf.sidecar_table
        )
        .expect("write to String is infallible");
    }

    sql.push_str(" WHERE m.owner_principal_kind = $1 AND m.owner_principal_id = $2");

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
        sql.push_str(" AND m.schema_id = $3");
    }

    if matches!(req.supersession, SupersessionStatus::HeadsOnly) {
        if let Some(sf) = &stateful {
            // Head-by-natural-key: latest memories.created_at per NK tuple
            // under the same schema_id. docs/03 §Stateful Fact schemas.
            let nk_pairs = sf
                .natural_key_columns
                .iter()
                .map(|c| format!("s2.{c} = sf.{c}"))
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
        .bind(owner_principal_id);
    if let Some(sid) = &schema_id_filter {
        q = q.bind(sid.clone());
    }
    // Bind (schema_id, schema_version) values for the CASE expression
    // in payload projection. Multiple schema versions can share the
    // same schema_id while living in different sidecar tables.
    for schema in &schemas_with_sidecar {
        q = q.bind(schema.schema_id.as_str().to_string());
        q = q.bind(schema.schema_version.into_inner().cast_signed());
    }

    let rows: Vec<MemoryRowDb> = q
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;

    let memories: Vec<MemoryRow> = rows
        .into_iter()
        .map(memory_row_from_db)
        .collect::<Result<Vec<_>, _>>()?;

    let seq_high_water = read_seq_high_water(pool, owner_kind, owner_principal_id).await?;

    Ok(QueryResponse {
        memories,
        seq_high_water,
    })
}

fn memory_row_from_db(r: MemoryRowDb) -> Result<MemoryRow, StorageError> {
    let schema_version = u32::try_from(r.schema_version).map_err(|_| {
        StorageError::Internal(format!(
            "invalid memory schema_version {} for memory {}",
            r.schema_version, r.memory_id
        ))
    })?;

    Ok(MemoryRow {
        id: MemoryId::new(r.memory_id),
        kind: match r.kind.as_deref() {
            Some("Abstraction") => EntityKind::Abstraction,
            Some("Perspective") => EntityKind::Perspective,
            _ => EntityKind::Fact,
        },
        schema_id: SchemaId::new(r.schema_id),
        schema_version: SchemaVersion::new(schema_version),
        owner: Owner {
            principal: match r.owner_principal_kind.as_str() {
                "User" => Principal::User(UserId::new(r.owner_principal_id)),
                _ => Principal::Group(GroupId::new(r.owner_principal_id)),
            },
            org_id: OrgId::new(r.owner_org_id),
        },
        payload: r.payload_json.map(String::into_bytes).unwrap_or_default(),
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
    schema_version: i32,
    kind: Option<String>,
    event_id: Option<Vec<u8>>,
    payload_json: Option<String>,
}

async fn read_seq_high_water(
    pool: &PgPool,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
) -> Result<Option<uuid::Uuid>, StorageError> {
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT seq FROM proxima_core.change_event \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2 \
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
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
    PgIdent::table(&sf.sidecar_table)?;
    if sf.natural_key_columns.is_empty() {
        return Err(StorageError::Internal(
            "stateful_heads with empty natural_key_columns".into(),
        ));
    }
    for col in &sf.natural_key_columns {
        PgIdent::column(col)?;
    }
    Ok(sf)
}
