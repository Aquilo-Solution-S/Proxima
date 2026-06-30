use proxima_core::{EdgeAuthorshipKind, EntityKind};
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphValidityViolation {
    MissingEndpoint {
        edge_id: uuid::Uuid,
    },
    InvalidDerivedProvenance {
        memory_id: uuid::Uuid,
    },
    NonStrictDerivationTime {
        edge_id: uuid::Uuid,
        source_memory_id: uuid::Uuid,
        target_memory_id: uuid::Uuid,
    },
    InvalidOperatorEdgeShape {
        edge_id: uuid::Uuid,
    },
}

/// Collect runtime witnesses for Lean `MemoryGraphValid` / `EdgeCoreValid` drift.
///
/// This is intentionally a test fixture, not a production verifier. Production
/// paths validate before write; this scanner lets integration tests assert that
/// fixtures/migrations did not admit graph rows that violate the kernel shape.
///
/// # Errors
///
/// Returns SQL errors from the backing test database.
pub async fn collect_memory_graph_violations(
    pool: &PgPool,
) -> Result<Vec<GraphValidityViolation>, sqlx::Error> {
    let mut violations = Vec::new();
    violations.extend(missing_endpoint_violations(pool).await?);
    violations.extend(missing_derived_provenance_violations(pool).await?);
    violations.extend(non_strict_derivation_time_violations(pool).await?);
    violations.extend(operator_edge_shape_violations(pool).await?);
    Ok(violations)
}

async fn missing_endpoint_violations(
    pool: &PgPool,
) -> Result<Vec<GraphValidityViolation>, sqlx::Error> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT e.edge_id
           FROM proxima_core.edges e
          WHERE (e.source_memory_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM proxima_core.memories m WHERE m.memory_id = e.source_memory_id AND m.tombstoned_at IS NULL))
             OR (e.target_memory_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM proxima_core.memories m WHERE m.memory_id = e.target_memory_id AND m.tombstoned_at IS NULL))
             OR (e.source_goal_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM proxima_core.goals g WHERE g.goal_id = e.source_goal_id AND g.state <> 'Abandoned'))
             OR (e.target_goal_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM proxima_core.goals g WHERE g.goal_id = e.target_goal_id AND g.state <> 'Abandoned'))
             OR (e.source_fact_entity_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM proxima_core.fact_entities f WHERE f.fact_entity_id = e.source_fact_entity_id))
             OR (e.target_fact_entity_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM proxima_core.fact_entities f WHERE f.fact_entity_id = e.target_fact_entity_id))",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(edge_id,)| GraphValidityViolation::MissingEndpoint { edge_id })
        .collect())
}

async fn missing_derived_provenance_violations(
    pool: &PgPool,
) -> Result<Vec<GraphValidityViolation>, sqlx::Error> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT m.memory_id
           FROM proxima_core.memories m
          WHERE m.kind IS NOT NULL
            AND m.tombstoned_at IS NULL
            AND NOT EXISTS (
                SELECT 1
                  FROM proxima_core.edges e
                 WHERE e.source_memory_id = m.memory_id
                   AND e.relation_class = 'Provenance'
                   AND e.target_memory_id IS NOT NULL
            )",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(memory_id,)| GraphValidityViolation::InvalidDerivedProvenance { memory_id })
        .collect())
}

async fn non_strict_derivation_time_violations(
    pool: &PgPool,
) -> Result<Vec<GraphValidityViolation>, sqlx::Error> {
    let rows: Vec<(uuid::Uuid, uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
        "SELECT e.edge_id, e.source_memory_id, e.target_memory_id
           FROM proxima_core.edges e
           JOIN proxima_core.memories source ON source.memory_id = e.source_memory_id
           JOIN proxima_core.memories target ON target.memory_id = e.target_memory_id
          WHERE e.relation_class IN ('Provenance', 'Supersession')
            AND e.source_memory_id IS NOT NULL
            AND e.target_memory_id IS NOT NULL
            AND target.created_at >= source.created_at",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(edge_id, source_memory_id, target_memory_id)| {
            GraphValidityViolation::NonStrictDerivationTime {
                edge_id,
                source_memory_id,
                target_memory_id,
            }
        })
        .collect())
}

async fn operator_edge_shape_violations(
    pool: &PgPool,
) -> Result<Vec<GraphValidityViolation>, sqlx::Error> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT e.edge_id
           FROM proxima_core.edges e
           LEFT JOIN proxima_core.memories source ON source.memory_id = e.source_memory_id
           LEFT JOIN proxima_core.memories target ON target.memory_id = e.target_memory_id
           LEFT JOIN proxima_core.goals source_goal ON source_goal.goal_id = e.source_goal_id
          WHERE CASE e.authorship_kind
                WHEN 'OperatorFtoA' THEN (e.relation_class = 'Provenance' AND source.kind = 'Abstraction' AND target.kind IS NULL) IS NOT TRUE
                WHEN 'OperatorAtoA' THEN (e.relation_class = 'Provenance' AND source.kind = 'Abstraction' AND target.kind = 'Abstraction') IS NOT TRUE
                WHEN 'OperatorAtoP' THEN (e.relation_class = 'Provenance' AND source.kind = 'Perspective' AND target.kind = 'Abstraction') IS NOT TRUE
                WHEN 'OperatorAtoGoal' THEN (e.relation_class = 'Structural' AND source_goal.goal_id IS NOT NULL AND target.kind = 'Abstraction') IS NOT TRUE
                ELSE FALSE
                END",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(edge_id,)| GraphValidityViolation::InvalidOperatorEdgeShape { edge_id })
        .collect())
}

#[allow(dead_code)]
fn _assert_public_types_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<GraphValidityViolation>();
    let _ = EntityKind::Fact;
    let _ = EdgeAuthorshipKind::OperatorFtoA;
}
