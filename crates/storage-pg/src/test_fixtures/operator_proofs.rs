use proxima_core::EntityKind;
use sqlx::PgPool;

/// One way the stored graph can disagree with the kernel shape.
///
/// An edge has no id, so a witness names the row by its key: the endpoints
/// and the kind. That is not a loss of precision — the key IS the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphValidityViolation {
    /// E1: an endpoint the edge points at does not exist (or is tombstoned /
    /// abandoned, which is how a live graph read sees "gone").
    MissingEndpoint { edge: EdgeKey },
    /// A derived memory that declares nothing at all: no origin it was made
    /// from and no reference it interprets. Every A/P row is one or the
    /// other, and both leave index rows behind.
    UngroundedDerived { memory_id: uuid::Uuid },
    /// Lean N1 `derivationTimeStrict`: an origin must be strictly older than
    /// the row it grounds.
    NonStrictDerivationTime { edge: EdgeKey },
    /// E3: `ℓ(source) ≥ ℓ(target)` for memory endpoints.
    LayeringViolation { edge: EdgeKey },
    /// E2: the row is owned by the source owner, always.
    OwnerIsNotSourceOwner { edge: EdgeKey },
}

/// An edge's whole identity.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct EdgeKey {
    pub source_kind: String,
    pub source_id: uuid::Uuid,
    pub target_kind: String,
    pub target_id: uuid::Uuid,
    pub kind: String,
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
    // SQL-POLICY: fixed-fragment — `sql` is one of the module's own `&'static
    // str` scan constants; no value reaches it from a caller.
    sqlx::query_as::<_, EdgeKey>(sqlx::AssertSqlSafe(sql))
        .fetch_all(pool)
        .await
}

const MISSING_ENDPOINT_SQL: &str = "
SELECT e.source_kind::text AS source_kind, e.source_id,
       e.target_kind::text AS target_kind, e.target_id, e.kind::text AS kind
  FROM proxima_core.edges e
 WHERE NOT EXISTS (
           SELECT 1 FROM (
               SELECT memory_id AS entity_id FROM proxima_core.memories WHERE tombstoned_at IS NULL
               UNION ALL SELECT goal_id FROM proxima_core.goals WHERE state <> 'Abandoned'
               UNION ALL SELECT fact_entity_id FROM proxima_core.fact_entities
           ) eo WHERE eo.entity_id = e.source_id
       )
    OR NOT EXISTS (
           SELECT 1 FROM (
               SELECT memory_id AS entity_id FROM proxima_core.memories WHERE tombstoned_at IS NULL
               UNION ALL SELECT goal_id FROM proxima_core.goals WHERE state <> 'Abandoned'
               UNION ALL SELECT fact_entity_id FROM proxima_core.fact_entities
           ) eo WHERE eo.entity_id = e.target_id
       )";

const NON_STRICT_DERIVATION_TIME_SQL: &str = "
SELECT e.source_kind::text AS source_kind, e.source_id,
       e.target_kind::text AS target_kind, e.target_id, e.kind::text AS kind
  FROM proxima_core.edges e
  JOIN proxima_core.memories source ON source.memory_id = e.source_id
  JOIN proxima_core.memories target ON target.memory_id = e.target_id
 WHERE e.kind = 'origin'::proxima_core.edge_kind
   AND target.created_at >= source.created_at";

const LAYERING_SQL: &str = "
SELECT e.source_kind::text AS source_kind, e.source_id,
       e.target_kind::text AS target_kind, e.target_id, e.kind::text AS kind
  FROM proxima_core.edges e
 WHERE proxima_core.edge_endpoint_layer(e.source_kind) IS NOT NULL
   AND proxima_core.edge_endpoint_layer(e.target_kind) IS NOT NULL
   AND proxima_core.edge_endpoint_layer(e.source_kind)
       < proxima_core.edge_endpoint_layer(e.target_kind)";

const OWNER_SQL: &str = "
SELECT e.source_kind::text AS source_kind, e.source_id,
       e.target_kind::text AS target_kind, e.target_id, e.kind::text AS kind
  FROM proxima_core.edges e
  JOIN (
      SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories
      UNION ALL SELECT goal_id, owner_kind, owner_id FROM proxima_core.goals
      UNION ALL SELECT fact_entity_id, owner_kind, owner_id FROM proxima_core.fact_entities
  ) seo ON seo.entity_id = e.source_id
 WHERE seo.owner_kind <> e.owner_kind
    OR seo.owner_id IS DISTINCT FROM e.owner_id";

async fn ungrounded_derived_memory_ids(pool: &PgPool) -> Result<Vec<uuid::Uuid>, sqlx::Error> {
    let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT m.memory_id
           FROM proxima_core.memories m
          WHERE m.kind IS NOT NULL
            AND m.tombstoned_at IS NULL
            AND NOT EXISTS (
                SELECT 1 FROM proxima_core.edges e WHERE e.source_id = m.memory_id
            )",
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
}
