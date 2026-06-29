use std::fmt::Write as _;

use proxima_core::verbs::query::{GoalRow, QueryRequest, SupersessionStatus};
use proxima_core::{OwnerRefKind, StorageError};
use sqlx::PgPool;

use crate::error::internal;

use super::rows::{GoalRowDb, goal_row_from_db};

pub(super) async fn query_goals(
    pool: &PgPool,
    req: &QueryRequest,
    read_owner_kinds: &[OwnerRefKind],
    read_owner_ids: &[Option<uuid::Uuid>],
    schema_id_filter: Option<&str>,
) -> Result<Vec<GoalRow>, StorageError> {
    let goal_ids: Vec<uuid::Uuid> = req.goal_ids.iter().map(|id| id.into_inner()).collect();
    let id_hydration =
        !req.memory_ids.is_empty() || !req.goal_ids.is_empty() || !req.edge_ids.is_empty();
    if id_hydration && goal_ids.is_empty() {
        return Ok(Vec::new());
    }
    let payload_projection = if req.include_payloads {
        "g.payload"
    } else {
        "''::bytea"
    };
    let mut sql = format!(
        "SELECT g.goal_id, g.schema_id, g.schema_version, \
                g.owner_kind, g.owner_id, \
                g.title, g.text, g.state, \
                g.supersedes, {payload_projection} AS payload, \
                COALESCE(array_agg(e.target_goal_id) FILTER \
                    (WHERE e.target_goal_id IS NOT NULL), '{{}}'::uuid[]) AS dependency_goal_ids \
         FROM proxima_core.goals g \
         LEFT JOIN proxima_core.edges e \
           ON e.source_goal_id = g.goal_id \
          AND e.relation = 'core/depends-on' \
          AND e.target_goal_id IS NOT NULL \
         WHERE EXISTS (
             SELECT 1
               FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[]) AS s(kind, id)
              WHERE g.owner_kind = s.kind
                AND g.owner_id IS NOT DISTINCT FROM s.id
         )",
    );
    // Bindings: $1=owner_kind, $2=owner_id; the remaining
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
    sql.push_str(
        " GROUP BY g.goal_id, g.owner_kind, g.owner_id \
          ORDER BY g.created_at DESC LIMIT ",
    );
    sql.push_str(&u64::from(req.limit).to_string());

    let mut q = sqlx::query_as::<_, GoalRowDb>(&sql)
        .bind(read_owner_kinds)
        .bind(read_owner_ids);
    if let Some(sid) = schema_id_filter {
        q = q.bind(sid.to_string());
    }
    if !goal_ids.is_empty() {
        q = q.bind(goal_ids);
    }
    let rows = q.fetch_all(pool).await.map_err(internal)?;
    rows.into_iter().map(goal_row_from_db).collect()
}
