use proxima_core::EntityKind;
use sqlx::PgPool;

/// One way the stored graph can disagree with the kernel shape.
///
/// Pins live on `memory.origins` / `memory.refs`. A witness names the pin
/// by source `t`, target `t`, and kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphValidityViolation {
    MissingEndpoint { edge: EdgeKey },
    UngroundedDerived { memory_id: uuid::Uuid },
    NonStrictDerivationTime { edge: EdgeKey },
    LayeringViolation { edge: EdgeKey },
    OwnerIsNotSourceOwner { edge: EdgeKey },
}

/// A pin's identity: source t, target t, kind.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct EdgeKey {
    pub source_kind: String,
    pub source_id: uuid::Uuid,
    pub target_kind: String,
    pub target_id: uuid::Uuid,
    pub kind: String,
}

/// Collect runtime witnesses for Lean `MemoryGraphValid` / `EdgeValid` drift.
///
/// # Errors
///
/// Returns SQL errors from the backing test database.
pub async fn collect_memory_graph_violations(
    pool: &PgPool,
) -> Result<Vec<GraphValidityViolation>, sqlx::Error> {
    let mut violations = Vec::new();
    violations.extend(
        edge_keys(pool, MISSING_ENDPOINT_SQL)
            .await?
            .into_iter()
            .map(|edge| GraphValidityViolation::MissingEndpoint { edge }),
    );
    violations.extend(
        ungrounded_derived_memory_ids(pool)
            .await?
            .into_iter()
            .map(|memory_id| GraphValidityViolation::UngroundedDerived { memory_id }),
    );
    violations.extend(
        edge_keys(pool, NON_STRICT_DERIVATION_TIME_SQL)
            .await?
            .into_iter()
            .map(|edge| GraphValidityViolation::NonStrictDerivationTime { edge }),
    );
    violations.extend(
        edge_keys(pool, LAYERING_SQL)
            .await?
            .into_iter()
            .map(|edge| GraphValidityViolation::LayeringViolation { edge }),
    );
    violations.extend(
        edge_keys(pool, OWNER_SQL)
            .await?
            .into_iter()
            .map(|edge| GraphValidityViolation::OwnerIsNotSourceOwner { edge }),
    );
    Ok(violations)
}

async fn edge_keys(pool: &PgPool, sql: &'static str) -> Result<Vec<EdgeKey>, sqlx::Error> {
    sqlx::query_as::<_, EdgeKey>(sqlx::AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
}

const PINS: &str = "
SELECT src.kind::text AS source_kind, src.t AS source_id,
       tgt.kind::text AS target_kind, pin AS target_id, pins.kind
  FROM (
        SELECT t, unnest(origins) AS pin, 'origin'::text AS kind FROM proxima_core.memory
        UNION ALL
        SELECT t, unnest(refs), 'reference' FROM proxima_core.memory
       ) pins
  JOIN proxima_core.memory src ON src.t = pins.t
  JOIN proxima_core.memory tgt ON tgt.t = pins.pin
";

const MISSING_ENDPOINT_SQL: &str = "
SELECT src.kind::text AS source_kind, src.t AS source_id,
       COALESCE(tgt.kind::text, '') AS target_kind, pin AS target_id, pins.kind
  FROM (
        SELECT t, unnest(origins) AS pin, 'origin'::text AS kind FROM proxima_core.memory
        UNION ALL
        SELECT t, unnest(refs), 'reference' FROM proxima_core.memory
       ) pins
  JOIN proxima_core.memory src ON src.t = pins.t
  LEFT JOIN proxima_core.memory tgt ON tgt.t = pins.pin
 WHERE tgt.t IS NULL";

const NON_STRICT_DERIVATION_TIME_SQL: &str = "
SELECT src.kind::text AS source_kind, src.t AS source_id,
       tgt.kind::text AS target_kind, pin AS target_id, pins.kind
  FROM (
        SELECT t, unnest(origins) AS pin, 'origin'::text AS kind FROM proxima_core.memory
       ) pins
  JOIN proxima_core.memory src ON src.t = pins.t
  JOIN proxima_core.memory tgt ON tgt.t = pins.pin
 WHERE src.t <= tgt.t";

const LAYERING_SQL: &str = "
SELECT src.kind::text AS source_kind, src.t AS source_id,
       tgt.kind::text AS target_kind, pin AS target_id, pins.kind
  FROM (
        SELECT t, unnest(origins) AS pin, 'origin'::text AS kind FROM proxima_core.memory
        UNION ALL
        SELECT t, unnest(refs), 'reference' FROM proxima_core.memory
       ) pins
  JOIN proxima_core.memory src ON src.t = pins.t
  JOIN proxima_core.memory tgt ON tgt.t = pins.pin
 WHERE CASE src.kind::text
           WHEN 'fact' THEN 0
           WHEN 'abstraction' THEN 1
           ELSE 2
       END
     < CASE tgt.kind::text
           WHEN 'fact' THEN 0
           WHEN 'abstraction' THEN 1
           ELSE 2
       END";

const OWNER_SQL: &str = "
SELECT src.kind::text AS source_kind, src.t AS source_id,
       tgt.kind::text AS target_kind, pin AS target_id, pins.kind
  FROM (
        SELECT t, unnest(origins) AS pin, 'origin'::text AS kind FROM proxima_core.memory
        UNION ALL
        SELECT t, unnest(refs), 'reference' FROM proxima_core.memory
       ) pins
  JOIN proxima_core.memory src ON src.t = pins.t
  JOIN proxima_core.memory tgt ON tgt.t = pins.pin
 WHERE src.owner_id IS DISTINCT FROM tgt.owner_id";

async fn ungrounded_derived_memory_ids(pool: &PgPool) -> Result<Vec<uuid::Uuid>, sqlx::Error> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT m.t
           FROM proxima_core.memory m
          WHERE m.kind <> 'fact'
            AND COALESCE(array_length(m.origins, 1), 0) = 0
            AND COALESCE(array_length(m.refs, 1), 0) = 0",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(memory_id,)| memory_id).collect())
}

#[allow(dead_code)]
fn _assert_public_types_are_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<GraphValidityViolation>();
    let _ = EntityKind::Fact;
    let _ = PINS;
}
