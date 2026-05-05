use std::fmt::Write as _;

use proxima_core::verbs::query::{
    EntityKind, MemoryRow, QueryRequest, QueryResponse, SupersessionStatus,
};
use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
use proxima_core::{Principal, StorageError};
use sqlx::PgPool;

use crate::pg_ident::PgIdent;

use super::edges::query_edges;
use super::goals::query_goals;
use super::rows::{MemoryRowDb, memory_row_from_db, read_seq_high_water, validate_stateful_filter};

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
