//! Postgres storage port impls.
//!
//! The verb logic lives under [`verbs`]; this module wires the
//! `PgStorage` struct, connection lifecycle, and migration runner,
//! then delegates each narrow storage port method to its per-verb
//! implementation.
#[cfg(any(test, feature = "test-fixtures"))]
extern crate self as proxima_storage_pg;

#[doc(hidden)]
pub use proxima_core as core;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use proxima_core::change_event::{EdgeTargetProjection, EntityRef};
use proxima_core::compliance::{
    ComplianceAuditContext, ComplianceEraseOutcome, EraseAuthorization,
};
use proxima_core::read_models::{
    AbstractionRow, ActiveGoalSummary, ChangeEventForWake, FactRow, GoalWakeCandidate,
    GoalWakeCandidateRequest, MemorySnapshot, SidecarSpec,
};
use proxima_core::storage_ports::{
    ChangeEventPort, CitationPort, ComplianceErasePort, EdgeReadPort, EmbeddingJobPort,
    EmbeddingTextPort, EmbeddingWritePort, FactIngestPort, FactRetentionPort, GoalReadPort,
    GoalWakeCandidatePort, GoalWritePort, McpCallReadPort, McpCallWritePort, MemoryAuthoringPort,
    MemoryInspectPort, MemoryReadPort, OwnerAccessReadPort, OwnerMembershipAdminPort,
    OwnerTransferPort, RegistryProjectionPort, SourceBatchPort, StoragePorts,
};
use proxima_core::verbs::change_history::{ChangeHistoryRequest, ChangeHistoryResponse};
use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWithCitation, AuthorizedFactWrite, FactIngestOutcome, FactWriteCommand,
};
use proxima_core::verbs::goal_write::{
    AchieveGoalAtomicRequest, CreateGoalAtomicRequest, DecomposeGoalAtomicRequest,
    DecomposeGoalOutcome, GoalWriteOutcome, ModifyGoalAtomicRequest, TransitionGoalAtomicRequest,
};
use proxima_core::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
use proxima_core::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use proxima_core::verbs::query::{
    EdgeExistsRequest, EdgeExistsResponse, EdgeReadRequest, EdgeReadResponse, FactCitationReadback,
    MemoryLineageRequest, MemoryLineageResponse, MemorySearchRequest, MemorySearchResult,
    QueryRequest, QueryResponse,
};
use proxima_core::{
    AuthorDerivedOutcome, AuthorDerivedRequest, DerivedEdgeSpec, EdgeAuthorshipKind,
    EdgeEndpointKindRow, EdgeId, EmbeddingJobClaim, EntityId, EntityKind, FactEntityId,
    FactSourceBatchRow, GroupId, MembershipRow, MemoryDependency, MemoryGraphPayloadRow, MemoryId,
    MemoryKindRow, NeighborEdgeRow, Owner, OwnerRef, Relation, RelationClass, SchemaId,
    SchemaVersion, SourceBatchId, SourceId, StorageError, UserId,
};
use proxima_core::{EmbeddableEntityRef, EmbeddingWriteOutcome, SidecarPayload};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Postgres, Transaction};
pub use verbs::fact_embeddings::{
    EmbeddingInlineDrainOutcome, EmbeddingReconcileOptions, EmbeddingReconcileOutcome,
    EmbeddingReconcileScope,
};

use crate::error::internal;

type StoredEdgeProofRow = (
    String,
    EntityKind,
    uuid::Uuid,
    EntityKind,
    uuid::Uuid,
    EdgeAuthorshipKind,
    Option<uuid::Uuid>,
);

#[doc(hidden)]
pub mod access;
mod authorship;
mod change_event;
mod error;
mod pg_ident;
mod pgvector;
pub mod sidecars;
pub mod query {
    pub use crate::verbs::query::{
        MAX_SNAPSHOT_EDGES, authorized_code_chunk_head_candidates, fact_entity_id_for,
    };
}
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_fixtures;
pub mod verbs;
/// Stable, discoverable re-export of the exported `OwnerAccessPort` adapter
/// (see [`access::PgOwnerAccessResolver`]) for embedding hosts.
pub use access::PgOwnerAccessResolver;
pub use sidecars::{
    PgSidecarKey, PgSidecarRegistry, PgSidecarRegistryFrozen, core_pg_sidecars,
    register_core_pg_sidecars,
};

/// Default DB URL when `DATABASE_URL` is unset. Matches the
/// dev DB created locally via `createdb proxima_dev`.
pub const DEFAULT_DATABASE_URL: &str = "postgres://postgres@localhost/proxima_dev";

const NEIGHBOR_MEMORY_EDGES_SQL: &str = "
WITH read_set(owner_kind, owner_id) AS (
    SELECT * FROM unnest($1::proxima_core.owner_ref_kind[], $2::uuid[])
)
SELECT e.edge_id, e.relation,
       e.source_kind,
       COALESCE(e.source_memory_id, sfe.current_memory_id) AS source_memory_id,
       e.target_kind,
       COALESCE(e.target_memory_id, tfe.current_memory_id) AS target_memory_id,
       EXISTS (
           SELECT 1
             FROM read_set rs
            WHERE rs.owner_kind = COALESCE(tm.owner_kind, tg.owner_kind)
              AND rs.owner_id IS NOT DISTINCT FROM COALESCE(tm.owner_id, tg.owner_id)
       ) AS target_visible,
       COALESCE(sm.owner_kind, sg.owner_kind) = $3
       AND COALESCE(sm.owner_id, sg.owner_id) IS NOT DISTINCT FROM $4 AS source_world_visible
  FROM proxima_core.edges e
  LEFT JOIN proxima_core.fact_entities sfe
    ON sfe.fact_entity_id = e.source_fact_entity_id
  LEFT JOIN proxima_core.fact_entities tfe
    ON tfe.fact_entity_id = e.target_fact_entity_id
  LEFT JOIN proxima_core.memories sm
    ON sm.memory_id = COALESCE(e.source_memory_id, sfe.current_memory_id)
  LEFT JOIN proxima_core.goals sg
    ON sg.goal_id = e.source_goal_id
  LEFT JOIN proxima_core.memories tm
    ON tm.memory_id = COALESCE(e.target_memory_id, tfe.current_memory_id)
  LEFT JOIN proxima_core.goals tg
    ON tg.goal_id = e.target_goal_id
 WHERE EXISTS (
           SELECT 1
             FROM read_set rs
            WHERE rs.owner_kind = COALESCE(sm.owner_kind, sg.owner_kind)
              AND rs.owner_id IS NOT DISTINCT FROM COALESCE(sm.owner_id, sg.owner_id)
       )
   AND (e.source_memory_id = ANY($5::uuid[])
        OR e.target_memory_id = ANY($5::uuid[])
        OR sfe.current_memory_id = ANY($5::uuid[])
        OR tfe.current_memory_id = ANY($5::uuid[]))
   AND NOT (
        COALESCE(sm.owner_kind, sg.owner_kind) = $3
        AND COALESCE(sm.owner_id, sg.owner_id) IS NOT DISTINCT FROM $4
        AND NOT EXISTS (
            SELECT 1
              FROM read_set rs
             WHERE rs.owner_kind = COALESCE(tm.owner_kind, tg.owner_kind)
               AND rs.owner_id IS NOT DISTINCT FROM COALESCE(tm.owner_id, tg.owner_id)
        )
   )
 ORDER BY e.edge_id DESC
 LIMIT $6
";

/// Migration versions deleted from the v0.0.4 destructive baseline.
///
/// `SQLx` stores core and flavor migrations in one `public._sqlx_migrations`
/// table. Keep this explicit list in sync between stale-DB preflight and the
/// guarded local reset path; do not delete broad version ranges.
pub const RETIRED_PRE_V004_MIGRATION_VERSIONS: &[i64] = &[2, 3, 4, 5, 6, 7, 20_260_622_000_000];

/// Embedded core migration set under `crates/storage-pg/migrations/`.
///
/// `ignore_missing = true` is load-bearing when the same database also
/// records flavor migrations in `SQLx`'s default `_sqlx_migrations` table.
#[must_use]
pub fn core_migrator() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator
}

/// Fail closed before `SQLx` checksum/missing-file behavior when a database
/// contains pre-v0.0.4 Proxima storage artifacts.
///
/// # Errors
///
/// Returns [`StorageError::V004ResetRequired`] for stale schema state and
/// [`StorageError::Internal`] for catalog query failures.
///
/// # Panics
///
/// Panics if the embedded core migrator does not contain baseline version 1.
pub async fn ensure_v004_baseline_compatible(pool: &PgPool) -> Result<(), StorageError> {
    let migration_table_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(pool)
            .await
            .map_err(internal)?;

    let proxima_schema_objects: Vec<String> = sqlx::query_scalar(
        "SELECT table_schema || '.' || table_name
           FROM information_schema.tables
          WHERE table_schema IN ('proxima_core', 'proxima_code')
          ORDER BY table_schema, table_name
          LIMIT 20",
    )
    .fetch_all(pool)
    .await
    .map_err(internal)?;

    let mut old_versions = Vec::new();
    let mut checksum_mismatch = false;
    let mut current_v1_seen = false;
    if migration_table_exists {
        let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT version, checksum
               FROM public._sqlx_migrations
              WHERE success
                AND version = ANY($1::bigint[])
              ORDER BY version",
        )
        .bind(
            std::iter::once(1_i64)
                .chain(RETIRED_PRE_V004_MIGRATION_VERSIONS.iter().copied())
                .collect::<Vec<_>>(),
        )
        .fetch_all(pool)
        .await
        .map_err(internal)?;

        let current_v1_checksum = core_migrator()
            .iter()
            .find(|migration| migration.version == 1)
            .expect("core baseline migration version 1 exists")
            .checksum
            .as_ref()
            .to_vec();
        for (version, checksum) in rows {
            if version == 1 {
                current_v1_seen = true;
                checksum_mismatch = checksum != current_v1_checksum;
            } else {
                old_versions.push(version);
            }
        }
    }

    let untracked_proxima_schema =
        !proxima_schema_objects.is_empty() && (!migration_table_exists || !current_v1_seen);

    if !untracked_proxima_schema && old_versions.is_empty() && !checksum_mismatch {
        return Ok(());
    }

    let mut details = Vec::new();
    if untracked_proxima_schema {
        details.push(format!(
            "pre-existing Proxima schema objects without v0.0.4 baseline marker: {}",
            proxima_schema_objects.join(", ")
        ));
    }
    if !old_versions.is_empty() {
        details.push(format!("old migration versions: {old_versions:?}"));
    }
    if checksum_mismatch {
        details.push("version 1 checksum differs from v0.0.4 baseline".to_string());
    }
    Err(StorageError::V004ResetRequired {
        details: details.join("; "),
    })
}

#[derive(Debug, Clone)]
pub struct PgStorage {
    pool: PgPool,
    sidecars: PgSidecarRegistryFrozen,
}

impl PgStorage {
    /// Connect using `url`, build a tuned pool, and verify
    /// connectivity by acquiring one connection.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Unavailable` on connection or
    /// query failure.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        let opts: PgConnectOptions = url.parse().map_err(|e: sqlx::Error| {
            StorageError::Unavailable(format!("invalid DATABASE_URL: {e}"))
        })?;
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(opts)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        // Validate connectivity with a trivial query.
        sqlx::query!("SELECT 1 AS one")
            .fetch_one(&pool)
            .await
            .map_err(|e| StorageError::Unavailable(e.to_string()))?;

        Ok(Self {
            pool,
            sidecars: core_pg_sidecars(),
        })
    }

    /// Read `DATABASE_URL` from env, fallback to
    /// `DEFAULT_DATABASE_URL`. Convenience for the bin / dev.
    #[must_use]
    pub fn url_from_env() -> String {
        std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_string())
    }

    #[cfg(any(
        test,
        feature = "test-fixtures",
        feature = "backend-api",
        debug_assertions
    ))]
    #[doc(hidden)]
    #[must_use]
    pub fn pool_for_tests(&self) -> &PgPool {
        &self.pool
    }

    #[cfg(any(feature = "backend-api", feature = "test-fixtures"))]
    #[doc(hidden)]
    #[must_use]
    pub fn clone_pool_for_backend(&self) -> PgPool {
        self.pool.clone()
    }

    #[must_use]
    pub fn sidecars(&self) -> &PgSidecarRegistryFrozen {
        &self.sidecars
    }

    /// Replace the entire sidecar registry.
    ///
    /// The caller must include the core sidecars. The boot/facade path
    /// enforces sidecar coverage with `freeze_against`; tests may pass
    /// deliberate partial registries.
    #[must_use]
    pub fn with_sidecars(mut self, sidecars: PgSidecarRegistryFrozen) -> Self {
        self.sidecars = sidecars;
        self
    }

    #[must_use]
    pub fn storage_ports(self: Arc<Self>) -> StoragePorts {
        StoragePorts::builder()
            .fact_ingest(self.clone())
            .mcp_call_write(self.clone())
            .mcp_call_read(self.clone())
            .memory_authoring(self.clone())
            .memory_read(self.clone())
            .memory_inspect(self.clone())
            .embedding_text(self.clone())
            .embedding_write(self.clone())
            .embedding_job(self.clone())
            .goal_write(self.clone())
            .goal_read(self.clone())
            .change_event(self.clone())
            .edge_read(self.clone())
            .citation(self.clone())
            .owner_access_read(self.clone())
            .owner_membership_admin(self.clone())
            .owner_transfer(self.clone())
            .source_batch(self.clone())
            .fact_retention(self.clone())
            .compliance_erase(self.clone())
            .registry_projection(self)
            .build()
    }

    /// Global enqueue-only embedding reconciliation.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the reconciliation query.
    pub async fn reconcile_embeddings(
        &self,
        options: EmbeddingReconcileOptions<'_>,
    ) -> Result<EmbeddingReconcileOutcome, StorageError> {
        verbs::fact_embeddings::reconcile_embeddings(&self.pool, options).await
    }

    /// Inline drain for queued embedding jobs.
    ///
    /// # Errors
    ///
    /// Returns storage errors from claiming or writing jobs/embeddings.
    pub async fn drain_embedding_jobs_inline(
        &self,
        client: &dyn proxima_core::llm::EmbeddingClient,
        limit: i64,
    ) -> Result<EmbeddingInlineDrainOutcome, StorageError> {
        verbs::fact_embeddings::drain_embedding_jobs_inline(&self.pool, client, limit).await
    }

    /// Apply all pending migrations under
    /// `crates/storage-pg/migrations/`. Idempotent — sqlx tracks
    /// applied migrations in `_sqlx_migrations`. Call once
    /// at process start before any verb dispatch.
    ///
    /// `ignore_missing = true` matches the per-flavor migrator
    /// (`flavors/*/migrations.rs`): core and every flavor share the
    /// default `_sqlx_migrations` table, so on a second run the core
    /// migrator sees flavor-authored versions it doesn't know about.
    /// Without this relaxation the second run fails with
    /// `VersionMissing(<flavor version>)`. The core version-set is
    /// still validated; we only relax the cross-author check.
    ///
    /// # Errors
    ///
    /// Returns `StorageError::Internal` on any sqlx
    /// migration failure (broken file, conflict with the
    /// recorded checksum, etc.).
    pub async fn run_migrations(&self) -> Result<(), StorageError> {
        ensure_v004_baseline_compatible(&self.pool).await?;
        core_migrator().run(&self.pool).await.map_err(internal)?;
        Ok(())
    }
}

fn edge_draft_from_spec<'a>(edge: &'a DerivedEdgeSpec<'a>) -> verbs::edge_append::EdgeDraft<'a> {
    verbs::edge_append::EdgeDraft {
        edge_id: uuid::Uuid::now_v7(),
        relation: edge.relation,
        source_kind: edge.source_kind,
        source_memory_id: Some(edge.source_memory_id.into_inner()),
        source_goal_id: None,
        source_fact_entity_id: None,
        target_kind: edge.target_kind,
        target_memory_id: Some(edge.target_memory_id.into_inner()),
        target_goal_id: None,
        target_fact_entity_id: None,
        authorship_kind: edge.authorship_kind,
        authorship_owner_memory_id: edge.authorship_owner_memory_id.map(MemoryId::into_inner),
        owner: edge.owner,
    }
}

fn operator_edge_authorship_values() -> [&'static str; 4] {
    [
        proxima_core::EdgeAuthorshipKind::OperatorFtoA.as_str(),
        proxima_core::EdgeAuthorshipKind::OperatorAtoA.as_str(),
        proxima_core::EdgeAuthorshipKind::OperatorAtoP.as_str(),
        proxima_core::EdgeAuthorshipKind::OperatorAtoGoal.as_str(),
    ]
}

fn operator_proof_edges<'a>(req: &'a AuthorDerivedRequest<'a>) -> Vec<&'a DerivedEdgeSpec<'a>> {
    let expected_authorship = req.operator_kind.edge_authorship();
    req.edges
        .iter()
        .filter(|edge| {
            edge.source_memory_id == req.memory_id && edge.authorship_kind == expected_authorship
        })
        .collect()
}

#[allow(clippy::too_many_lines)] // storage-side proof validation is one linear SQL-backed invariant check
async fn validate_author_derived_storage_request(
    tx: &mut Transaction<'_, Postgres>,
    req: &AuthorDerivedRequest<'_>,
) -> Result<(), StorageError> {
    let proof_edges = operator_proof_edges(req);
    if proof_edges.is_empty() {
        return Err(StorageError::ConstraintViolation(
            "operator invocation inputs must be nonempty".into(),
        ));
    }

    let expected_input_kind = req.operator_kind.phase().input_kind();
    let expected_authorship = req.operator_kind.edge_authorship();
    for edge in req.edges {
        if edge.source_memory_id == req.memory_id
            && edge.relation.descriptor.class == RelationClass::Provenance
            && edge.authorship_kind.is_operator()
            && edge.authorship_kind != expected_authorship
        {
            return Err(StorageError::ConstraintViolation(
                "operator provenance edge authorship kind does not match operator phase".into(),
            ));
        }
    }
    let mut input_ids = Vec::with_capacity(proof_edges.len());
    let mut seen = BTreeSet::new();
    for edge in &proof_edges {
        if edge.relation.descriptor.class != RelationClass::Provenance {
            return Err(StorageError::ConstraintViolation(
                "operator proof edges must use Provenance relations".into(),
            ));
        }
        if edge.source_kind != req.kind {
            return Err(StorageError::ConstraintViolation(
                "operator proof edge source kind must match output kind".into(),
            ));
        }
        if edge.target_kind != expected_input_kind {
            return Err(StorageError::ConstraintViolation(format!(
                "operator proof edge target kind must be {expected_input_kind:?}"
            )));
        }
        if edge.authorship_kind != expected_authorship {
            return Err(StorageError::ConstraintViolation(
                "operator proof edge authorship kind does not match operator phase".into(),
            ));
        }
        if !seen.insert(edge.target_memory_id) {
            return Err(StorageError::ConstraintViolation(
                "operator invocation inputs must be unique".into(),
            ));
        }
        input_ids.push(edge.target_memory_id.into_inner());
    }

    let rows: Vec<(uuid::Uuid, Option<EntityKind>, Option<uuid::Uuid>, bool)> = sqlx::query_as(
        "SELECT m.memory_id, m.kind, fr.source_batch_id, sb.closed_at IS NOT NULL
           FROM proxima_core.memories m
           LEFT JOIN proxima_core.fact_receipts fr ON fr.receipt_id = m.receipt_id
           LEFT JOIN proxima_core.source_batches sb ON sb.id = fr.source_batch_id
          WHERE m.memory_id = ANY($1::uuid[])
            AND m.tombstoned_at IS NULL",
    )
    .bind(&input_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(crate::error::map_err)?;

    if rows.len() != input_ids.len() {
        return Err(StorageError::ConstraintViolation(
            "operator invocation inputs must exist and be live".into(),
        ));
    }

    let mut ftoa_batch = None;
    for (memory_id, actual_kind, source_batch_id, closed) in rows {
        let actual = actual_kind.unwrap_or(EntityKind::Fact);
        if actual != expected_input_kind {
            return Err(StorageError::ConstraintViolation(format!(
                "invalid input kind for {:?}: expected {expected_input_kind:?}, got {actual:?}",
                MemoryId::new(memory_id)
            )));
        }
        if req.operator_kind == proxima_core::MemoryOperatorKind::FtoA {
            let source_batch_id = source_batch_id.ok_or_else(|| {
                StorageError::ConstraintViolation(
                    "F→A operator inputs must be receipted Facts".into(),
                )
            })?;
            if !closed {
                return Err(StorageError::ConstraintViolation(
                    "F→A operator source batch must be closed".into(),
                ));
            }
            match ftoa_batch {
                Some(existing) if existing != source_batch_id => {
                    return Err(StorageError::ConstraintViolation(
                        "F→A operator inputs must belong to one source batch".into(),
                    ));
                }
                Some(_) => {}
                None => ftoa_batch = Some(source_batch_id),
            }
        }
    }

    if req.operator_kind == proxima_core::MemoryOperatorKind::FtoA {
        let expected = ftoa_batch.ok_or_else(|| {
            StorageError::ConstraintViolation("F→A operator source batch is required".into())
        })?;
        if req.source_batch_id.map(SourceBatchId::into_inner) != Some(expected) {
            return Err(StorageError::ConstraintViolation(
                "F→A operator source_batch_id must match input Facts".into(),
            ));
        }
    } else if req.source_batch_id.is_some() {
        return Err(StorageError::ConstraintViolation(
            "source_batch_id is only valid for F→A operator invocations".into(),
        ));
    }

    Ok(())
}

async fn validate_author_derived_replay_edges(
    tx: &mut Transaction<'_, Postgres>,
    req: &AuthorDerivedRequest<'_>,
) -> Result<(), StorageError> {
    let expected_authorship = req.operator_kind.edge_authorship();
    let expected = req
        .edges
        .iter()
        .filter(|edge| {
            edge.source_memory_id == req.memory_id
                && edge.authorship_kind == expected_authorship
                && edge.relation.descriptor.class == RelationClass::Provenance
        })
        .map(|edge| {
            (
                edge.relation.descriptor.relation.as_str().to_string(),
                format!("{:?}", edge.source_kind),
                edge.source_memory_id.into_inner(),
                format!("{:?}", edge.target_kind),
                edge.target_memory_id.into_inner(),
                format!("{:?}", edge.authorship_kind),
                edge.authorship_owner_memory_id.map(MemoryId::into_inner),
            )
        })
        .collect::<BTreeSet<_>>();
    let rows: Vec<StoredEdgeProofRow> = sqlx::query_as(
        "SELECT relation, source_kind, source_memory_id, target_kind, target_memory_id,
                authorship_kind, authorship_owner_memory_id
           FROM proxima_core.edges
          WHERE source_memory_id = $1
            AND relation_class = 'Provenance'
            AND authorship_kind::text = ANY($2::text[])",
    )
    .bind(req.memory_id.into_inner())
    .bind(operator_edge_authorship_values())
    .fetch_all(&mut **tx)
    .await
    .map_err(crate::error::map_err)?;
    let actual = rows
        .into_iter()
        .map(
            |(
                relation,
                source_kind,
                source_memory_id,
                target_kind,
                target_memory_id,
                authorship_kind,
                authorship_owner_memory_id,
            )| {
                (
                    relation,
                    format!("{source_kind:?}"),
                    source_memory_id,
                    format!("{target_kind:?}"),
                    target_memory_id,
                    format!("{authorship_kind:?}"),
                    authorship_owner_memory_id,
                )
            },
        )
        .collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(StorageError::Conflict(
            "derived memory idempotent replay edge proof mismatch".into(),
        ))
    }
}

#[async_trait::async_trait]
impl FactIngestPort for PgStorage {
    async fn ingest_fact_atomic(
        &self,
        owner: &Owner,
        draft: &FactWriteCommand,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        verbs::fact_ingest::ingest_fact_atomic(&self.pool, owner, draft, embedding_model_id).await
    }

    async fn ingest_fact_with_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWrite,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;
        let fact_sidecars = self.sidecars.clone();
        let payload = sidecar_payload.clone();
        let outcome = verbs::fact_ingest::ingest_fact_with_sidecar_in_tx(
            &mut tx,
            authorized,
            embedding_model_id,
            move |tx, outcome| {
                Box::pin(async move {
                    fact_sidecars
                        .insert_memory_sidecar(tx, outcome.memory_id, &payload)
                        .await
                })
            },
        )
        .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(outcome)
    }

    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;
        let sidecars = self.sidecars.clone();
        let fact_sidecars = sidecars.clone();
        let payload = sidecar_payload.clone();
        let outcome = verbs::fact_ingest::ingest_fact_with_citation_in_tx(
            &mut tx,
            &sidecars,
            authorized,
            embedding_model_id,
            move |tx, outcome| {
                Box::pin(async move {
                    fact_sidecars
                        .insert_memory_sidecar(tx, outcome.memory_id, &payload)
                        .await
                })
            },
        )
        .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(outcome)
    }
}

#[async_trait::async_trait]
impl McpCallWritePort for PgStorage {
    async fn persist_mcp_call_atomic(
        &self,
        input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError> {
        verbs::persist_mcp_call::persist_mcp_call_atomic(&self.pool, input).await
    }
}

#[async_trait::async_trait]
impl McpCallReadPort for PgStorage {
    async fn read_mcp_call_history(
        &self,
        req: &McpCallHistoryRequest,
    ) -> Result<McpCallHistoryResponse, StorageError> {
        verbs::mcp_call_history::read_mcp_call_history(&self.pool, req).await
    }
}

#[async_trait::async_trait]
impl MemoryAuthoringPort for PgStorage {
    async fn author_derived(
        &self,
        req: &AuthorDerivedRequest<'_>,
        _proof: proxima_core::storage_ports::OperatorWriteProof,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        let mut tx = self.pool.begin().await.map_err(internal)?;
        validate_author_derived_storage_request(&mut tx, req).await?;
        let draft = verbs::derive_append::DerivedDraft {
            memory_id: req.memory_id.into_inner(),
            owner: req.owner,
            kind: req.kind,
            schema_id: req.schema_id.clone(),
            schema_version: req.schema_version,
            text: req.text.clone(),
            operator_kind: req.operator_kind,
            operator_id: req.operator_id,
            input_contract_id: req.input_contract_id,
            source_batch_id: req.source_batch_id,
            model_id: req.model_id,
            prompt_version: req.prompt_version,
            supersedes: req.supersedes,
            embedding: req.embedding.clone(),
            embedding_model_id: req.embedding_model_id,
        };
        let sidecars = self.sidecars.clone();
        let sidecar_payload = req.sidecar_payload.clone();
        let outcome =
            verbs::derive_append::append_derived_in_tx(&mut tx, &draft, move |tx, outcome| {
                Box::pin(async move {
                    sidecars
                        .insert_memory_sidecar(tx, outcome.memory_id, &sidecar_payload)
                        .await
                })
            })
            .await?;
        let mut edge_count = 0;
        if outcome.idempotent_replay {
            validate_author_derived_replay_edges(&mut tx, req).await?;
        } else {
            for edge in req.edges {
                let draft = edge_draft_from_spec(edge);
                if let Some(sidecar_payload) = edge.sidecar_payload {
                    let sidecars = self.sidecars.clone();
                    let payload = sidecar_payload.clone();
                    verbs::edge_append::append_edge_with_sidecar_in_tx(
                        tx.as_mut(),
                        &draft,
                        move |tx, edge_id| {
                            Box::pin(async move {
                                sidecars.insert_edge_sidecar(tx, edge_id, &payload).await
                            })
                        },
                    )
                    .await?;
                } else {
                    verbs::edge_append::append_edge_in_tx(tx.as_mut(), &draft).await?;
                }
                edge_count += 1;
            }
        }
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(AuthorDerivedOutcome {
            memory_id: outcome.memory_id,
            idempotent_replay: outcome.idempotent_replay,
            edge_count,
        })
    }

    async fn append_memory_edge(
        &self,
        edge: &DerivedEdgeSpec<'_>,
        _proof: proxima_core::storage_ports::EdgeWriteProof,
    ) -> Result<EdgeId, StorageError> {
        if edge.authorship_kind.is_operator() {
            return Err(StorageError::ConstraintViolation(
                "operator-authored edges require an operator proof-carrier write path".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(internal)?;
        let draft = edge_draft_from_spec(edge);
        let edge_id = EdgeId::new(draft.edge_id);
        if let Some(sidecar_payload) = edge.sidecar_payload {
            let sidecars = self.sidecars.clone();
            let payload = sidecar_payload.clone();
            verbs::edge_append::append_edge_with_sidecar_in_tx(
                tx.as_mut(),
                &draft,
                move |tx, edge_id| {
                    Box::pin(
                        async move { sidecars.insert_edge_sidecar(tx, edge_id, &payload).await },
                    )
                },
            )
            .await?;
        } else {
            verbs::edge_append::append_edge_in_tx(tx.as_mut(), &draft).await?;
        }
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(edge_id)
    }

    async fn load_memory_kinds(
        &self,
        owner: &Owner,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<MemoryKindRow>, StorageError> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = memory_ids
            .iter()
            .copied()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>();
        let (owner_kind, owner_id) = owner.columns();
        let rows: Vec<(uuid::Uuid, Option<proxima_core::EntityKind>)> = sqlx::query_as(
            "SELECT m.memory_id, m.kind
             FROM proxima_core.memories m
             WHERE m.owner_kind = $1
               AND m.owner_id IS NOT DISTINCT FROM $2
               AND m.memory_id = ANY($3::uuid[])",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(|(memory_id, kind)| MemoryKindRow {
                memory_id: MemoryId::new(memory_id),
                kind,
            })
            .collect())
    }

    async fn load_fact_source_batches(
        &self,
        _owner: &Owner,
        memory_ids: &[MemoryId],
    ) -> Result<Vec<FactSourceBatchRow>, StorageError> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = memory_ids
            .iter()
            .copied()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>();
        let rows: Vec<(uuid::Uuid, uuid::Uuid)> = sqlx::query_as(
            "SELECT m.memory_id, fr.source_batch_id
             FROM proxima_core.memories m
             JOIN proxima_core.fact_receipts fr ON fr.receipt_id = m.receipt_id
             WHERE m.kind IS NULL
               AND m.tombstoned_at IS NULL
               AND m.memory_id = ANY($1::uuid[])",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(|(memory_id, source_batch_id)| FactSourceBatchRow {
                memory_id: MemoryId::new(memory_id),
                source_batch_id: SourceBatchId::new(source_batch_id),
            })
            .collect())
    }

    async fn load_memory_edge_ids(
        &self,
        _owner: &Owner,
        relation: &str,
        source_memory_id: MemoryId,
        target_memory_ids: &[MemoryId],
    ) -> Result<Vec<EdgeId>, StorageError> {
        if target_memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let target_ids = target_memory_ids
            .iter()
            .copied()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>();
        let rows: Vec<uuid::Uuid> = sqlx::query_scalar(
            "SELECT edge_id
             FROM proxima_core.edges
             WHERE relation = $1
               AND source_memory_id = $2
               AND target_memory_id = ANY($3::uuid[])
             ORDER BY edge_id DESC",
        )
        .bind(relation)
        .bind(source_memory_id.into_inner())
        .bind(&target_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows.into_iter().map(EdgeId::new).collect())
    }
}

#[async_trait::async_trait]
impl MemoryReadPort for PgStorage {
    async fn load_fact_text(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<Option<String>, StorageError> {
        verbs::fact_embeddings::load_fact_text(&self.pool, owner, memory_id).await
    }

    async fn load_memory_graph_payloads(
        &self,
        owner: &Owner,
        memory_ids: &[MemoryId],
        include_body: bool,
    ) -> Result<Vec<MemoryGraphPayloadRow>, StorageError> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = memory_ids
            .iter()
            .copied()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>();
        let (owner_kind, owner_id) = owner.columns();
        let rows: Vec<(uuid::Uuid, Option<Vec<String>>, Option<String>)> = sqlx::query_as(
            "SELECT m.memory_id,
                    COALESCE(n.tags, d.tags) AS tags,
                    CASE WHEN $4
                         THEN COALESCE(n.body, d.body, m.text)
                         ELSE NULL
                    END AS body
             FROM proxima_core.memories m
             LEFT JOIN proxima_core.agent_note_v1 n USING (memory_id)
             LEFT JOIN proxima_core.agent_derivation_v1 d USING (memory_id)
             WHERE m.owner_kind = $1
               AND m.owner_id IS NOT DISTINCT FROM $2
               AND m.memory_id = ANY($3::uuid[])",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(&ids)
        .bind(include_body)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(|(memory_id, tags, body)| MemoryGraphPayloadRow {
                memory_id: MemoryId::new(memory_id),
                tags,
                body,
            })
            .collect())
    }

    async fn load_neighbor_memory_edges(
        &self,
        read_owners: &[OwnerRef],
        memory_ids: &[MemoryId],
        limit: usize,
    ) -> Result<Vec<NeighborEdgeRow>, StorageError> {
        if memory_ids.is_empty() {
            return Ok(Vec::new());
        }
        if read_owners.is_empty() {
            return Ok(Vec::new());
        }
        let ids = memory_ids
            .iter()
            .copied()
            .map(MemoryId::into_inner)
            .collect::<Vec<_>>();
        let limit = i64::try_from(limit).map_err(|err| StorageError::Internal(err.to_string()))?;
        let (read_owner_kinds, read_owner_ids) = verbs::query::read_owner_columns(read_owners);
        let (world_kind, world_id) =
            crate::access::owner_columns::owner_binds(&proxima_core::access::world());
        let rows: Vec<(
            uuid::Uuid,
            String,
            proxima_core::EntityKind,
            Option<uuid::Uuid>,
            proxima_core::EntityKind,
            Option<uuid::Uuid>,
            bool,
            bool,
        )> = sqlx::query_as(NEIGHBOR_MEMORY_EDGES_SQL)
            .bind(&read_owner_kinds)
            .bind(&read_owner_ids)
            .bind(world_kind)
            .bind(world_id)
            .bind(&ids)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    edge_id,
                    relation,
                    source_kind,
                    source_memory_id,
                    target_kind,
                    target_memory_id,
                    target_visible,
                    _source_world_visible,
                )| {
                    let target_memory_kind = target_visible.then_some(target_kind);
                    let target = if target_visible {
                        target_memory_id.map_or(EdgeTargetProjection::Unavailable, |id| {
                            EdgeTargetProjection::Visible {
                                target: EntityRef::Memory(MemoryId::new(id)),
                            }
                        })
                    } else {
                        EdgeTargetProjection::Redacted
                    };
                    NeighborEdgeRow {
                        edge_id: EdgeId::new(edge_id),
                        relation,
                        source_kind,
                        source_memory_id: source_memory_id.map(MemoryId::new),
                        target_memory_kind,
                        target,
                    }
                },
            )
            .collect())
    }

    async fn load_edge_endpoint_kinds(
        &self,
        edge_ids: &[EdgeId],
    ) -> Result<Vec<EdgeEndpointKindRow>, StorageError> {
        if edge_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids = edge_ids
            .iter()
            .copied()
            .map(EdgeId::into_inner)
            .collect::<Vec<_>>();
        let rows: Vec<(
            uuid::Uuid,
            proxima_core::EntityKind,
            proxima_core::EntityKind,
        )> = sqlx::query_as(
            "SELECT edge_id, source_kind, target_kind
                 FROM proxima_core.edges
                 WHERE edge_id = ANY($1::uuid[])",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .into_iter()
            .map(|(edge_id, source_kind, target_kind)| EdgeEndpointKindRow {
                edge_id: EdgeId::new(edge_id),
                source_kind,
                target_kind: Some(target_kind),
            })
            .collect())
    }

    async fn query_memories(
        &self,
        req: &QueryRequest,
        schemas: &[proxima_core::verbs::schema::SchemaInfo],
    ) -> Result<QueryResponse, StorageError> {
        verbs::query::query_memories(&self.pool, &self.sidecars, req, schemas).await
    }

    async fn search_memories(
        &self,
        req: &MemorySearchRequest,
        projections: &[proxima_core::verbs::schema::MemorySearchProjection],
    ) -> Result<Vec<MemorySearchResult>, StorageError> {
        verbs::query::search_memories(&self.pool, req, projections).await
    }

    async fn walk_memory_lineage(
        &self,
        read_owners: &[OwnerRef],
        req: &MemoryLineageRequest,
    ) -> Result<MemoryLineageResponse, StorageError> {
        verbs::query::walk_memory_lineage(&self.pool, read_owners, req).await
    }
}

#[async_trait::async_trait]
impl MemoryInspectPort for PgStorage {
    async fn load_memory_by_id(
        &self,
        memory_id: proxima_core::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Option<MemorySnapshot>, StorageError> {
        verbs::consolidate::load_memory_by_id(&self.pool, &self.sidecars, memory_id, sidecars).await
    }

    async fn list_memory_dependencies(
        &self,
        owner: &Owner,
        source_memory_id: MemoryId,
    ) -> Result<Vec<MemoryDependency>, StorageError> {
        verbs::consolidate::list_memory_dependencies(&self.pool, owner, source_memory_id).await
    }
}

#[async_trait::async_trait]
impl EmbeddingTextPort for PgStorage {
    async fn load_embedding_text(
        &self,
        owner: &Owner,
        entity_kind: proxima_core::EntityKind,
        memory_id: MemoryId,
    ) -> Result<Option<String>, StorageError> {
        verbs::fact_embeddings::load_embedding_text(&self.pool, owner, entity_kind, memory_id).await
    }

    async fn list_facts_missing_embedding(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryId>, StorageError> {
        verbs::fact_embeddings::list_facts_missing_embedding(&self.pool, owner, model_id, limit)
            .await
    }
}

#[async_trait::async_trait]
impl EmbeddingWritePort for PgStorage {
    async fn insert_embedding(
        &self,
        owner: &Owner,
        entity: EmbeddableEntityRef,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        _proof: proxima_core::storage_ports::EmbeddingWriteProof,
    ) -> Result<EmbeddingWriteOutcome, StorageError> {
        let mut tx =
            self.pool.begin().await.map_err(|err| {
                StorageError::Internal(format!("begin embedding insert tx: {err}"))
            })?;
        let outcome =
            verbs::fact_embeddings::insert_embedding(&mut tx, owner, entity, model_id, dim, vec)
                .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(outcome)
    }

    async fn upsert_fact_embedding(
        &self,
        owner: &Owner,
        memory_id: MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        proof: proxima_core::storage_ports::EmbeddingWriteProof,
    ) -> Result<(), StorageError> {
        self.insert_fact_embedding(owner, memory_id, model_id, dim, vec, proof)
            .await?;
        Ok(())
    }

    async fn upsert_memory_embedding(
        &self,
        owner: &Owner,
        entity_kind: proxima_core::EntityKind,
        memory_id: MemoryId,
        model_id: &str,
        dim: usize,
        vec: &[f32],
        proof: proxima_core::storage_ports::EmbeddingWriteProof,
    ) -> Result<(), StorageError> {
        self.insert_memory_embedding(owner, entity_kind, memory_id, model_id, dim, vec, proof)
            .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl EmbeddingJobPort for PgStorage {
    async fn claim_pending_embedding_jobs(
        &self,
        model_id: &str,
        limit: i64,
    ) -> Result<Vec<EmbeddingJobClaim>, StorageError> {
        verbs::fact_embeddings::claim_pending_embedding_jobs(&self.pool, model_id, limit).await
    }

    async fn complete_embedding_job(&self, claim: &EmbeddingJobClaim) -> Result<(), StorageError> {
        verbs::fact_embeddings::complete_embedding_job(&self.pool, claim).await
    }

    async fn fail_embedding_job(
        &self,
        claim: &EmbeddingJobClaim,
        error: &str,
    ) -> Result<(), StorageError> {
        verbs::fact_embeddings::fail_embedding_job(&self.pool, claim, error).await
    }

    async fn enqueue_missing_embedding_jobs(
        &self,
        owner: &Owner,
        model_id: &str,
        limit: i64,
    ) -> Result<u64, StorageError> {
        verbs::fact_embeddings::enqueue_missing_embedding_jobs(&self.pool, owner, model_id, limit)
            .await
    }

    async fn count_pending_embedding_jobs(&self, owner: &Owner) -> Result<u64, StorageError> {
        verbs::fact_embeddings::count_pending_embedding_jobs(&self.pool, owner).await
    }
}

#[async_trait::async_trait]
impl GoalWritePort for PgStorage {
    async fn create_goal_atomic(
        &self,
        req: &CreateGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::create_goal_atomic(&self.pool, &self.sidecars, req).await
    }

    async fn transition_goal_atomic(
        &self,
        req: &TransitionGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::transition_goal_atomic(&self.pool, &self.sidecars, req).await
    }

    async fn achieve_goal_atomic(
        &self,
        req: &AchieveGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::achieve_goal_atomic(&self.pool, &self.sidecars, req).await
    }

    async fn modify_goal_atomic(
        &self,
        req: &ModifyGoalAtomicRequest<'_>,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::modify_goal_atomic(&self.pool, &self.sidecars, req).await
    }

    async fn decompose_goal_atomic(
        &self,
        req: &DecomposeGoalAtomicRequest<'_>,
    ) -> Result<DecomposeGoalOutcome, StorageError> {
        verbs::goal_write::decompose_goal_atomic(&self.pool, &self.sidecars, req).await
    }
}

#[async_trait::async_trait]
impl GoalReadPort for PgStorage {
    async fn list_active_goals(
        &self,
        read_owners: &[OwnerRef],
        self_perspective_memory_id: MemoryId,
        limit: usize,
    ) -> Result<Vec<ActiveGoalSummary>, StorageError> {
        verbs::active_goals::list_active_goals(
            &self.pool,
            read_owners,
            self_perspective_memory_id,
            limit,
        )
        .await
    }
}

#[async_trait::async_trait]
impl GoalWakeCandidatePort for PgStorage {
    async fn list_goal_wake_candidates(
        &self,
        req: &GoalWakeCandidateRequest<'_>,
    ) -> Result<Vec<GoalWakeCandidate>, StorageError> {
        verbs::goal_wake_candidates::list_goal_wake_candidates(&self.pool, req).await
    }
}

#[async_trait::async_trait]
impl ChangeEventPort for PgStorage {
    async fn change_history(
        &self,
        read_owners: &[OwnerRef],
        req: &ChangeHistoryRequest,
    ) -> Result<ChangeHistoryResponse, StorageError> {
        verbs::change_history::change_history(&self.pool, read_owners, req).await
    }

    async fn list_change_events_after(
        &self,
        read_owners: &[OwnerRef],
        after: uuid::Uuid,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        verbs::consolidate::list_change_events_after(&self.pool, read_owners, after, limit).await
    }

    async fn list_change_events_for_replay(
        &self,
        owner: &Owner,
        after: uuid::Uuid,
        until: Option<uuid::Uuid>,
        limit: usize,
    ) -> Result<Vec<ChangeEventForWake>, StorageError> {
        verbs::consolidate::list_change_events_for_replay(&self.pool, owner, after, until, limit)
            .await
    }
}

#[async_trait::async_trait]
impl EdgeReadPort for PgStorage {
    async fn read_edges(
        &self,
        read_owners: &[OwnerRef],
        req: &EdgeReadRequest,
    ) -> Result<EdgeReadResponse, StorageError> {
        verbs::query::read_edges(&self.pool, read_owners, req).await
    }

    async fn edge_exists(
        &self,
        read_owners: &[OwnerRef],
        req: &EdgeExistsRequest,
    ) -> Result<EdgeExistsResponse, StorageError> {
        verbs::query::edge_exists(&self.pool, read_owners, req).await
    }
}

#[async_trait::async_trait]
impl CitationPort for PgStorage {
    async fn fact_entity_id_for(
        &self,
        owner: &Owner,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        natural_key: &[String],
    ) -> Result<Option<FactEntityId>, StorageError> {
        verbs::query::fact_entity_id_for_pool(
            &self.pool,
            owner,
            schema_id,
            schema_version,
            natural_key,
        )
        .await
    }

    async fn facts_citing_object(
        &self,
        read_owners: &[OwnerRef],
        cited_object_id: uuid::Uuid,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<MemorySnapshot>, StorageError> {
        verbs::query::facts_citing_object(
            &self.pool,
            &self.sidecars,
            read_owners,
            cited_object_id,
            sidecars,
        )
        .await
    }

    async fn citation_of_fact(
        &self,
        fact_memory_id: MemoryId,
    ) -> Result<Option<FactCitationReadback>, StorageError> {
        verbs::query::citation_of_fact(&self.pool, fact_memory_id).await
    }

    async fn citation_of_entity_head(
        &self,
        read_owners: &[OwnerRef],
        fact_entity_id: FactEntityId,
    ) -> Result<Option<FactCitationReadback>, StorageError> {
        verbs::query::citation_of_entity_head(&self.pool, read_owners, fact_entity_id).await
    }
}

#[async_trait::async_trait]
impl OwnerAccessReadPort for PgStorage {
    async fn resolve_membership(
        &self,
        member: &OwnerRef,
    ) -> Result<Vec<MembershipRow>, StorageError> {
        access::owner_columns::resolve_membership(&self.pool, member).await
    }

    async fn visible_to_any(
        &self,
        entity: EntityId,
        read_owners: &[OwnerRef],
    ) -> Result<bool, StorageError> {
        access::owner_columns::visible_to_any(&self.pool, entity, read_owners).await
    }

    async fn home_owner(&self, entity: EntityId) -> Result<Option<OwnerRef>, StorageError> {
        access::owner_columns::home_owner(&self.pool, entity).await
    }
}

#[async_trait::async_trait]
impl OwnerMembershipAdminPort for PgStorage {
    async fn add_group_member(
        &self,
        group_id: GroupId,
        member_user_id: UserId,
        relation: Relation,
        granted_by: uuid::Uuid,
    ) -> Result<(), StorageError> {
        access::owner_columns::add_group_member(
            &self.pool,
            group_id,
            member_user_id,
            relation,
            granted_by,
        )
        .await
    }

    async fn remove_group_member(
        &self,
        group_id: GroupId,
        member_user_id: UserId,
    ) -> Result<(), StorageError> {
        access::owner_columns::remove_group_member(&self.pool, group_id, member_user_id).await
    }

    async fn list_group_members(
        &self,
        group_id: GroupId,
    ) -> Result<Vec<(UserId, Relation)>, StorageError> {
        access::owner_columns::list_group_members(&self.pool, group_id).await
    }
}

#[async_trait::async_trait]
impl OwnerTransferPort for PgStorage {
    async fn transfer_to_world(
        &self,
        entity: EntityId,
        from_owner: OwnerRef,
    ) -> Result<bool, StorageError> {
        access::owner_columns::transfer_to_world(&self.pool, entity, from_owner).await
    }
}

#[async_trait::async_trait]
impl SourceBatchPort for PgStorage {
    async fn close_batch(
        &self,
        principal: &OwnerRef,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        verbs::close_batch::close_batch(&self.pool, principal, source_batch_id).await
    }
}

#[async_trait::async_trait]
impl FactRetentionPort for PgStorage {
    async fn upsert_fact_retention(&self, owner: &Owner, seconds: i64) -> Result<(), StorageError> {
        verbs::fact_retention::upsert_fact_retention(&self.pool, owner, seconds).await
    }

    async fn get_fact_retention(&self, owner: &Owner) -> Result<Option<i64>, StorageError> {
        verbs::fact_retention::get_fact_retention(&self.pool, owner).await
    }

    async fn clear_fact_retention(&self, owner: &Owner) -> Result<bool, StorageError> {
        verbs::fact_retention::clear_fact_retention(&self.pool, owner).await
    }
}

#[async_trait::async_trait]
impl ComplianceErasePort for PgStorage {
    async fn record_compliance_outcome(
        &self,
        audit: &ComplianceAuditContext,
        outcome: &ComplianceEraseOutcome,
    ) -> Result<(), StorageError> {
        verbs::compliance_erase::record_compliance_outcome(&self.pool, audit, outcome).await
    }

    async fn erase_group_owner_if_abandoned(
        &self,
        auth: &EraseAuthorization,
        group_id: GroupId,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<ComplianceEraseOutcome, StorageError> {
        verbs::compliance_erase::erase_group_owner_if_abandoned(
            &self.pool,
            auth,
            group_id,
            fact_sidecar_tables,
            goal_sidecar_tables,
            edge_sidecar_tables,
            citation_mapping_sidecar_tables,
            cited_object_sidecar_tables,
        )
        .await
    }

    async fn erase_personal_owner_if_drop_verified(
        &self,
        auth: &EraseAuthorization,
        user_id: UserId,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<ComplianceEraseOutcome, StorageError> {
        verbs::compliance_erase::erase_personal_owner_if_drop_verified(
            &self.pool,
            auth,
            user_id,
            fact_sidecar_tables,
            goal_sidecar_tables,
            edge_sidecar_tables,
            citation_mapping_sidecar_tables,
            cited_object_sidecar_tables,
        )
        .await
    }

    async fn erase_group_source_scope_if_owner_abandoned(
        &self,
        auth: &EraseAuthorization,
        group_id: GroupId,
        source_id: &SourceId,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<ComplianceEraseOutcome, StorageError> {
        verbs::compliance_erase::erase_group_source_scope_if_owner_abandoned(
            &self.pool,
            auth,
            group_id,
            source_id,
            fact_sidecar_tables,
            goal_sidecar_tables,
            edge_sidecar_tables,
            citation_mapping_sidecar_tables,
            cited_object_sidecar_tables,
        )
        .await
    }

    async fn erase_personal_source_scope_if_drop_verified(
        &self,
        auth: &EraseAuthorization,
        user_id: UserId,
        source_id: &SourceId,
        fact_sidecar_tables: &[String],
        goal_sidecar_tables: &[String],
        edge_sidecar_tables: &[String],
        citation_mapping_sidecar_tables: &[String],
        cited_object_sidecar_tables: &[String],
    ) -> Result<ComplianceEraseOutcome, StorageError> {
        verbs::compliance_erase::erase_personal_source_scope_if_drop_verified(
            &self.pool,
            auth,
            user_id,
            source_id,
            fact_sidecar_tables,
            goal_sidecar_tables,
            edge_sidecar_tables,
            citation_mapping_sidecar_tables,
            cited_object_sidecar_tables,
        )
        .await
    }
}

#[async_trait::async_trait]
impl RegistryProjectionPort for PgStorage {
    async fn load_memory_batch_facts(
        &self,
        owner: &Owner,
        memory_id: proxima_core::MemoryId,
        sidecars: &[SidecarSpec],
    ) -> Result<Vec<FactRow>, StorageError> {
        verbs::consolidate::load_memory_batch_facts(
            &self.pool,
            &self.sidecars,
            owner,
            memory_id,
            sidecars,
        )
        .await
    }

    async fn load_abstraction_heads(
        &self,
        owner: &Owner,
        sidecars: &[SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        verbs::consolidate::load_abstraction_heads(
            &self.pool,
            &self.sidecars,
            owner,
            sidecars,
            limit,
        )
        .await
    }
}
