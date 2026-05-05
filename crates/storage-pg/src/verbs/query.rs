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
//! LEFT JOIN the sidecar, project the row into a typed JSON value,
//! then encode the wire payload as CBOR bytes.

use std::fmt::Write as _;

use proxima_core::verbs::goal_write::GoalState;
use proxima_core::verbs::query::{
    EdgeRow, EntityKind, GoalRow, MemoryRow, QueryRequest, QueryResponse, StatefulHeadsFilter,
    SupersessionStatus,
};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{
    EntityRef, GoalId, GroupId, MemoryId, OrgId, Owner, Principal, SchemaId, SchemaVersion,
    StorageError, UserId,
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
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();
    let schema_id_filter = req.schema_id.as_ref().map(|s| s.as_str().to_string());
    if matches!(req.entity_kind, Some(EntityKind::Goal)) {
        return Ok(QueryResponse {
            memories: Vec::new(),
            goals: query_goals(
                pool,
                req,
                owner_kind,
                owner_principal_id,
                schema_id_filter.as_deref(),
            )
            .await?,
            edges: Vec::new(),
            seq_high_water: read_seq_high_water(pool, owner_kind, owner_principal_id).await?,
        });
    }

    let stateful = req
        .stateful_heads
        .as_ref()
        .filter(|_| matches!(req.supersession, SupersessionStatus::HeadsOnly))
        .map(validate_stateful_filter)
        .transpose()?;

    // Build payload projection: for each F/A/P schema with a sidecar
    // table, LEFT JOIN the sidecar and emit a CASE expression that
    // picks the matching row value for CBOR encoding.
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

    // Bindings: $1=owner_kind, $2=owner_principal_id.
    // Schema-id literals for the payload-projection CASE always come
    // after optional filters.
    let mut next_param = 3;
    let schema_param = schema_id_filter.as_ref().map(|_| {
        let param = next_param;
        next_param += 1;
        param
    });
    let memory_ids: Vec<uuid::Uuid> = req.memory_ids.iter().map(|id| id.into_inner()).collect();
    let memory_ids_param = (!memory_ids.is_empty()).then(|| {
        let param = next_param;
        next_param += 1;
        param
    });
    let case_param_base = next_param;

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
        write!(sql, " AND m.schema_id = ${}", schema_param.unwrap())
            .expect("write to String is infallible");
    }

    if let Some(param) = memory_ids_param {
        write!(sql, " AND m.memory_id = ANY(${param})").expect("write to String is infallible");
    } else if id_hydration {
        sql.push_str(" AND false");
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
    if !memory_ids.is_empty() {
        q = q.bind(memory_ids);
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
        .map(|row| memory_row_from_db(row, schemas))
        .collect::<Result<Vec<_>, _>>()?;

    let goals = if req.entity_kind.is_none() || matches!(req.entity_kind, Some(EntityKind::Goal)) {
        query_goals(
            pool,
            req,
            owner_kind,
            owner_principal_id,
            schema_id_filter.as_deref(),
        )
        .await?
    } else {
        Vec::new()
    };
    let edges = query_edges(pool, req, owner_kind, owner_principal_id).await?;
    let seq_high_water = read_seq_high_water(pool, owner_kind, owner_principal_id).await?;

    Ok(QueryResponse {
        memories,
        goals,
        edges,
        seq_high_water,
    })
}

fn memory_row_from_db(r: MemoryRowDb, schemas: &[SchemaInfo]) -> Result<MemoryRow, StorageError> {
    let schema_version = u32::try_from(r.schema_version).map_err(|_| {
        StorageError::Internal(format!(
            "invalid memory schema_version {} for memory {}",
            r.schema_version, r.memory_id
        ))
    })?;

    let schema_id = SchemaId::new(r.schema_id);
    let schema_version = SchemaVersion::new(schema_version);
    let cbor_encoder = schemas
        .iter()
        .find(|s| s.schema_id == schema_id && s.schema_version == schema_version)
        .and_then(|s| s.cbor_encoder);

    Ok(MemoryRow {
        id: MemoryId::new(r.memory_id),
        kind: match r.kind.as_deref() {
            Some("Abstraction") => EntityKind::Abstraction,
            Some("Perspective") => EntityKind::Perspective,
            _ => EntityKind::Fact,
        },
        schema_id,
        schema_version,
        owner: Owner {
            principal: match r.owner_principal_kind.as_str() {
                "User" => Principal::User(UserId::new(r.owner_principal_id)),
                _ => Principal::Group(GroupId::new(r.owner_principal_id)),
            },
            org_id: OrgId::new(r.owner_org_id),
        },
        payload: r
            .payload_json
            .as_deref()
            .map(|text| json_text_to_cbor(text, cbor_encoder))
            .transpose()?
            .unwrap_or_default(),
    })
}

fn json_text_to_cbor(
    text: &str,
    encoder: Option<proxima_core::verbs::schema::PayloadCborEncoder>,
) -> Result<Vec<u8>, StorageError> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| StorageError::Internal(format!("invalid payload JSON projection: {e}")))?;
    if let Some(encode) = encoder {
        return encode(&value)
            .map_err(|e| StorageError::Internal(format!("CBOR payload encode failed: {e}")));
    }
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(&value, &mut bytes)
        .map_err(|e| StorageError::Internal(format!("CBOR payload encode failed: {e}")))?;
    Ok(bytes)
}

async fn query_goals(
    pool: &PgPool,
    req: &QueryRequest,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
    schema_id_filter: Option<&str>,
) -> Result<Vec<GoalRow>, StorageError> {
    let goal_ids: Vec<uuid::Uuid> = req.goal_ids.iter().map(|id| id.into_inner()).collect();
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();
    if id_hydration && goal_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut sql = String::from(
        "SELECT g.goal_id, g.schema_id, g.schema_version, g.owner_principal_kind, \
                g.owner_principal_id, g.owner_org_id, g.text, g.state, \
                g.supersedes, \
                COALESCE(array_agg(gp.parent_goal_id) FILTER \
                    (WHERE gp.parent_goal_id IS NOT NULL), '{}'::uuid[]) AS parent_goal_ids \
         FROM proxima_core.goals g \
         LEFT JOIN proxima_core.goal_parents gp ON gp.goal_id = g.goal_id \
         WHERE g.owner_principal_kind = $1 AND g.owner_principal_id = $2",
    );
    // Bindings: $1=owner_kind, $2=owner_principal_id; the remaining
    // params are pushed in order, so $3 always lands on whichever
    // optional filter is present first.
    let schema_param = schema_id_filter.map(|_| 3);
    let goal_ids_param = (!goal_ids.is_empty()).then(|| if schema_param.is_some() { 4 } else { 3 });

    if let Some(p) = schema_param {
        write!(sql, " AND g.schema_id = ${p}").expect("write to String is infallible");
    }
    if let Some(p) = goal_ids_param {
        write!(sql, " AND g.goal_id = ANY(${p})").expect("write to String is infallible");
    } else if matches!(req.supersession, SupersessionStatus::HeadsOnly) {
        sql.push_str(
            " AND NOT EXISTS (SELECT 1 FROM proxima_core.goals g2 \
                              WHERE g2.supersedes = g.goal_id)",
        );
    }
    sql.push_str(" GROUP BY g.goal_id ORDER BY g.created_at DESC LIMIT ");
    sql.push_str(&u64::from(req.limit).to_string());

    let mut q = sqlx::query_as::<_, GoalRowDb>(&sql)
        .bind(owner_kind)
        .bind(owner_principal_id);
    if let Some(sid) = schema_id_filter {
        q = q.bind(sid.to_string());
    }
    if !goal_ids.is_empty() {
        q = q.bind(goal_ids);
    }
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
    rows.into_iter().map(goal_row_from_db).collect()
}

async fn query_edges(
    pool: &PgPool,
    req: &QueryRequest,
    owner_kind: &str,
    owner_principal_id: uuid::Uuid,
) -> Result<Vec<EdgeRow>, StorageError> {
    let edge_ids = req.edge_ids.clone();
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();
    if id_hydration && edge_ids.is_empty() {
        return Ok(Vec::new());
    }
    if edge_ids.is_empty() && req.entity_kind.is_some() {
        return Ok(Vec::new());
    }
    let mut sql = String::from(
        "SELECT edge_id, relation, relation_class, source_memory_id, source_goal_id, \
                target_memory_id, target_goal_id, owner_principal_kind, \
                owner_principal_id, owner_org_id \
         FROM proxima_core.edges \
         WHERE owner_principal_kind = $1 AND owner_principal_id = $2",
    );
    if !edge_ids.is_empty() {
        sql.push_str(" AND edge_id = ANY($3)");
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT ");
    sql.push_str(&u64::from(req.limit).to_string());

    let mut q = sqlx::query_as::<_, EdgeRowDb>(&sql)
        .bind(owner_kind)
        .bind(owner_principal_id);
    if !edge_ids.is_empty() {
        q = q.bind(edge_ids);
    }
    let rows = q
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(e.to_string()))?;
    rows.into_iter().map(edge_row_from_db).collect()
}

fn goal_row_from_db(r: GoalRowDb) -> Result<GoalRow, StorageError> {
    let state = match r.state.as_str() {
        "Active" => GoalState::Active,
        "Paused" => GoalState::Paused,
        "Achieved" => GoalState::Achieved,
        "Abandoned" => GoalState::Abandoned,
        other => {
            return Err(StorageError::Internal(format!(
                "unknown goal state: {other}"
            )));
        }
    };
    let schema_version = u32::try_from(r.schema_version).map_err(|_| {
        StorageError::Internal(format!(
            "invalid goal schema_version {} for goal {}",
            r.schema_version, r.goal_id
        ))
    })?;
    Ok(GoalRow {
        id: GoalId::new(r.goal_id),
        schema_id: SchemaId::new(r.schema_id),
        schema_version: SchemaVersion::new(schema_version),
        owner: owner_from_parts(
            &r.owner_principal_kind,
            r.owner_principal_id,
            r.owner_org_id,
        ),
        text: r.text,
        state,
        parent_goal_ids: r.parent_goal_ids.into_iter().map(GoalId::new).collect(),
        supersedes: r.supersedes.map(GoalId::new),
        payload: Vec::new(),
    })
}

fn edge_row_from_db(r: EdgeRowDb) -> Result<EdgeRow, StorageError> {
    let source = entity_ref_from_endpoint(r.source_memory_id, r.source_goal_id)?;
    let target = entity_ref_from_endpoint(r.target_memory_id, r.target_goal_id)?;
    Ok(EdgeRow {
        id: r.edge_id,
        relation: r.relation,
        relation_class: r.relation_class,
        source,
        target,
        owner: owner_from_parts(
            &r.owner_principal_kind,
            r.owner_principal_id,
            r.owner_org_id,
        ),
        payload: Vec::new(),
    })
}

fn entity_ref_from_endpoint(
    memory_id: Option<uuid::Uuid>,
    goal_id: Option<uuid::Uuid>,
) -> Result<EntityRef, StorageError> {
    match (memory_id, goal_id) {
        (Some(m), None) => Ok(EntityRef::Memory(MemoryId::new(m))),
        (None, Some(g)) => Ok(EntityRef::Goal(GoalId::new(g))),
        _ => Err(StorageError::Internal(
            "edge endpoint columns violate CHECK constraint".into(),
        )),
    }
}

fn owner_from_parts(kind: &str, principal_id: uuid::Uuid, org_id: uuid::Uuid) -> Owner {
    Owner {
        principal: match kind {
            "User" => Principal::User(UserId::new(principal_id)),
            _ => Principal::Group(GroupId::new(principal_id)),
        },
        org_id: OrgId::new(org_id),
    }
}

#[derive(Debug, sqlx::FromRow)]
struct GoalRowDb {
    goal_id: uuid::Uuid,
    schema_id: String,
    schema_version: i32,
    owner_principal_kind: String,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
    text: String,
    state: String,
    supersedes: Option<uuid::Uuid>,
    parent_goal_ids: Vec<uuid::Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
struct EdgeRowDb {
    edge_id: uuid::Uuid,
    relation: String,
    relation_class: String,
    source_memory_id: Option<uuid::Uuid>,
    source_goal_id: Option<uuid::Uuid>,
    target_memory_id: Option<uuid::Uuid>,
    target_goal_id: Option<uuid::Uuid>,
    owner_principal_kind: String,
    owner_principal_id: uuid::Uuid,
    owner_org_id: uuid::Uuid,
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
