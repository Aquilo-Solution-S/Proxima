use proxima_core::verbs::query::EntityKind;
use proxima_core::{
    AbstractionPayload, AuthzContext, FactPayload, GoalId, MemoryId, Owner, SchemaId, ToolError,
};
use proxima_storage_pg::query::{
    ChunkSeriesHead, CodeChunkVectorCandidate, CodeChunkVectorFilters, FileRevisionHeadRow,
    active_goals_for_memory_targets, nearest_code_chunk_candidates, owned_chunk_series_heads,
    owned_file_revision_heads, owned_present_file_revision_heads_except,
    readable_chunk_head_ts_for_file, readable_file_revision_head_ts,
};
use proxima_storage_pg::{PgSidecarRegistryFrozen, PgTuning};
use sqlx::PgPool;

use crate::payloads::{AcceptanceCriterionV1, AcceptanceVerifierKind, AcceptanceVerifierSpecV1};

/// Private code-flavor storage service passed to tools by the host.
///
/// Authz-filtered payload reads delegate to `proxima::flavor` (`&Engine`).
/// Code-series head / ANN helpers call `proxima_storage_pg::query` here —
/// they need the flavor's private pool and must not sit on the Flavor SDK.
/// `pool()` stays private (`from_backend_pool_for_host`/`for_tests`).
///
/// It also carries the boot's frozen sidecar registry. `erase_repo` is why:
/// the flavor deletes its own `proxima_code` rows and then hands the
/// admissions to a storage verb that walks the registry to reach whatever
/// else each one stamped. Composing a second registry inside the flavor
/// would be a second answer to a question the host has already answered.
#[derive(Clone)]
pub struct CodeFlavorStore {
    pool: PgPool,
    tuning: PgTuning,
    sidecars: PgSidecarRegistryFrozen,
}

impl std::fmt::Debug for CodeFlavorStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeFlavorStore").finish_non_exhaustive()
    }
}

impl CodeFlavorStore {
    #[cfg(feature = "host-api")]
    #[doc(hidden)]
    #[must_use]
    pub fn from_backend_pool_for_host(
        pool: PgPool,
        tuning: PgTuning,
        sidecars: PgSidecarRegistryFrozen,
    ) -> Self {
        Self {
            pool,
            tuning,
            sidecars,
        }
    }

    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    #[must_use]
    pub fn from_backend_pool_for_tests(pool: PgPool) -> Self {
        Self::from_backend_pool_with_tuning_for_tests(pool, PgTuning::default())
    }

    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    #[must_use]
    pub fn from_backend_pool_with_tuning_for_tests(pool: PgPool, tuning: PgTuning) -> Self {
        Self {
            pool,
            tuning,
            sidecars: test_sidecars(),
        }
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub(crate) fn sidecars(&self) -> &PgSidecarRegistryFrozen {
        &self.sidecars
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn authorized_memory_ids(
        &self,
        engine: &proxima_core::Engine,
        authz: &AuthzContext,
        owner: Owner,
        candidates: &[uuid::Uuid],
        entity_kind: EntityKind,
        schema_id: Option<SchemaId>,
        limit: usize,
    ) -> Result<Vec<MemoryId>, ToolError> {
        proxima::flavor::authorized_memory_ids(
            engine,
            authz,
            owner,
            candidates,
            entity_kind,
            schema_id,
            limit,
        )
        .await
    }

    pub(crate) async fn authorized_fact_payloads<P>(
        &self,
        engine: &proxima_core::Engine,
        authz: &AuthzContext,
        owner: Owner,
        candidates: &[uuid::Uuid],
        limit: usize,
    ) -> Result<Vec<(MemoryId, P)>, ToolError>
    where
        P: FactPayload + Clone,
    {
        proxima::flavor::authorized_fact_payloads::<P>(engine, authz, owner, candidates, limit)
            .await
    }

    pub(crate) async fn authorized_abstraction_payloads<P>(
        &self,
        engine: &proxima_core::Engine,
        authz: &AuthzContext,
        owner: Owner,
        candidates: &[uuid::Uuid],
        limit: usize,
    ) -> Result<Vec<(MemoryId, P)>, ToolError>
    where
        P: AbstractionPayload + Clone,
    {
        proxima::flavor::authorized_abstraction_payloads::<P>(
            engine, authz, owner, candidates, limit,
        )
        .await
    }

    /// Owner-only current file-revision heads of `repo_id` for `file_paths`.
    /// Head is `memory_head`; ingest compares these shas against git.
    /// Empty `file_paths` returns no rows.
    pub(crate) async fn owned_file_revision_heads(
        &self,
        owner: Owner,
        repo_id: uuid::Uuid,
        file_paths: &[String],
    ) -> Result<Vec<FileRevisionHeadRow>, ToolError> {
        owned_file_revision_heads(
            &self.pool,
            owner,
            &crate::payloads::FileRevisionV1::schema_id(),
            repo_id,
            file_paths,
        )
        .await
        .map_err(ToolError::Storage)
    }

    /// Owner-only `Present` heads whose path is not in `keep_paths`.
    pub(crate) async fn owned_present_file_revision_heads_except(
        &self,
        owner: Owner,
        repo_id: uuid::Uuid,
        keep_paths: &[String],
    ) -> Result<Vec<FileRevisionHeadRow>, ToolError> {
        owned_present_file_revision_heads_except(
            &self.pool,
            owner,
            &crate::payloads::FileRevisionV1::schema_id(),
            repo_id,
            keep_paths,
        )
        .await
        .map_err(ToolError::Storage)
    }

    /// Current file-revision `t`s for one path across the caller's
    /// read-owner set, own rows first.
    pub(crate) async fn readable_file_revision_head_ts(
        &self,
        owner: Owner,
        read_owners: &[Owner],
        repo_id: uuid::Uuid,
        file_path: &str,
    ) -> Result<Vec<uuid::Uuid>, ToolError> {
        readable_file_revision_head_ts(
            &self.pool,
            owner,
            read_owners,
            &crate::payloads::FileRevisionV1::schema_id(),
            repo_id,
            file_path,
        )
        .await
        .map_err(ToolError::Storage)
    }

    /// Owner-only current chunk series of one file (any state).
    pub(crate) async fn owned_chunk_series_heads(
        &self,
        owner: Owner,
        repo_id: uuid::Uuid,
        file_path: &str,
    ) -> Result<Vec<ChunkSeriesHead>, ToolError> {
        owned_chunk_series_heads(
            &self.pool,
            owner,
            &crate::payloads::CodeChunkV1::schema_id(),
            repo_id,
            file_path,
        )
        .await
        .map_err(ToolError::Storage)
    }

    /// Present chunk head `t`s for one file across the caller's read-owner
    /// set.
    pub(crate) async fn readable_chunk_head_ts_for_file(
        &self,
        owner: Owner,
        read_owners: &[Owner],
        repo_id: uuid::Uuid,
        file_path: &str,
    ) -> Result<Vec<uuid::Uuid>, ToolError> {
        readable_chunk_head_ts_for_file(
            &self.pool,
            owner,
            read_owners,
            &crate::payloads::CodeChunkV1::schema_id(),
            repo_id,
            file_path,
        )
        .await
        .map_err(ToolError::Storage)
    }

    /// Nearest `code-chunk-v1` chunks to a query embedding, best-first.
    ///
    /// Candidate producer: merge with lexical hits, then
    /// [`Self::authorized_abstraction_payloads`] (Query `HeadsOnly`).
    /// Embeddings live in `proxima_core.embeddings`, which flavor SQL
    /// may not join, so the query itself is backend-owned.
    pub(crate) async fn nearest_code_chunk_candidates(
        &self,
        owner: Owner,
        model_id: &str,
        query_embedding: &[f32],
        filters: CodeChunkVectorFilters<'_>,
        limit: usize,
    ) -> Result<Vec<CodeChunkVectorCandidate>, ToolError> {
        nearest_code_chunk_candidates(
            &self.pool,
            &self.tuning,
            owner,
            model_id,
            query_embedding,
            filters,
            i64::try_from(limit).unwrap_or(i64::MAX),
        )
        .await
        .map_err(ToolError::Storage)
    }

    /// Acceptance-criteria Facts for a work item, children in one JOIN.
    pub(crate) async fn acceptance_criteria_for_work_item(
        &self,
        work_item_t: uuid::Uuid,
    ) -> Result<Vec<CriteriaGroup>, ToolError> {
        let rows: Vec<CriteriaJoinRow> = sqlx::query_as(
            "SELECT c.t AS criteria_t,
                    r.criterion_key, r.description, r.required, r.verifier_kind,
                    r.verifier_path, r.verifier_command, r.verifier_pattern,
                    r.verifier_note
               FROM proxima_code.acceptance_criteria_v1 c
               LEFT JOIN proxima_code.acceptance_criterion_v1 r
                 ON r.criteria_memory_id = c.t
              WHERE c.work_item_memory_id = $1
              ORDER BY c.t ASC, r.criterion_index ASC NULLS LAST",
        )
        .bind(work_item_t)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(group_criteria_join_rows(rows))
    }

    /// Child criteria of one `test-requested` Fact.
    pub(crate) async fn test_requested_criteria(
        &self,
        test_requested_t: uuid::Uuid,
    ) -> Result<Vec<AcceptanceCriterionV1>, ToolError> {
        let rows: Vec<CriterionSqlRow> = sqlx::query_as(
            "SELECT criterion_key, description, required, verifier_kind,
                    verifier_path, verifier_command, verifier_pattern, verifier_note
               FROM proxima_code.test_requested_criterion_v1
              WHERE test_requested_memory_id = $1
              ORDER BY criterion_index ASC",
        )
        .bind(test_requested_t)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        Ok(rows.into_iter().map(AcceptanceCriterionV1::from).collect())
    }

    /// Active Goal heads that assign or evidence any of `targets`.
    ///
    /// One row per target that matches, Goal `t` chosen as
    /// `max(newest assignment, newest evidence)` — same as the old
    /// two `limit = 1` queries plus `.max()`.
    pub(crate) async fn active_goal_activations(
        &self,
        owner: Owner,
        targets: &[MemoryId],
    ) -> Result<Vec<(MemoryId, GoalId)>, ToolError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }
        let target_ids: Vec<uuid::Uuid> = targets.iter().map(|id| id.into_inner()).collect();
        let rows =
            active_goals_for_memory_targets(&self.pool, owner.stored_owner_id(), &target_ids)
                .await
                .map_err(ToolError::Storage)?;
        let mut out = Vec::new();
        for target in targets {
            let tid = target.into_inner();
            let assigned = rows
                .iter()
                .filter(|row| row.assignment_t == Some(tid))
                .map(|row| row.goal_id)
                .max();
            let evidenced = rows
                .iter()
                .filter(|row| row.evidence_t.contains(&tid))
                .map(|row| row.goal_id)
                .max();
            if let Some(goal_id) = [assigned, evidenced].into_iter().flatten().max() {
                out.push((*target, GoalId::new(goal_id)));
            }
        }
        Ok(out)
    }
}

/// The host's boot composition, repeated for a store built without a host.
///
/// Deliberately the same four steps `ProximaBuilder::boot` runs, in the same
/// order, so a fixture-built store answers `sidecars()` with the registry a
/// real deployment would have frozen. Test-only: production goes through
/// [`CodeFlavorStore::from_backend_pool_for_host`], which is handed the
/// boot's own registry.
#[cfg(any(test, debug_assertions))]
fn test_sidecars() -> PgSidecarRegistryFrozen {
    let mut registry = proxima_core::FlavorRegistry::new();
    crate::register(&mut registry).expect("the code flavor registers against a fresh registry");
    let registry = registry
        .try_freeze()
        .expect("core plus the code flavor freeze");
    let mut sidecars = proxima_storage_pg::PgSidecarRegistry::new();
    proxima_storage_pg::register_core_pg_sidecars(&mut sidecars);
    crate::register_pg_sidecars(&mut sidecars);
    sidecars
        .freeze_against(&registry)
        .expect("the code flavor's PG sidecars agree with its contract")
}

#[cfg(test)]
mod tuning_tests {
    use super::*;

    #[tokio::test]
    async fn store_carries_the_host_resolved_query_tuning() {
        tokio::task::yield_now().await;
        let tuning = PgTuning {
            hnsw_ef_search: 321,
            ..PgTuning::default()
        };
        let pool = PgPool::connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
        let store = CodeFlavorStore::from_backend_pool_with_tuning_for_tests(pool, tuning);

        assert_eq!(store.tuning, tuning);
    }
}

/// One acceptance-criteria Fact and its child rows.
#[derive(Debug, Clone)]
pub(crate) struct CriteriaGroup {
    pub memory_id: MemoryId,
    pub criteria: Vec<AcceptanceCriterionV1>,
}

#[derive(Debug, sqlx::FromRow)]
struct CriteriaJoinRow {
    criteria_t: uuid::Uuid,
    criterion_key: Option<String>,
    description: Option<String>,
    required: Option<bool>,
    verifier_kind: Option<AcceptanceVerifierKind>,
    verifier_path: Option<String>,
    verifier_command: Option<Vec<String>>,
    verifier_pattern: Option<String>,
    verifier_note: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct CriterionSqlRow {
    criterion_key: String,
    description: String,
    required: bool,
    verifier_kind: AcceptanceVerifierKind,
    verifier_path: Option<String>,
    verifier_command: Option<Vec<String>>,
    verifier_pattern: Option<String>,
    verifier_note: Option<String>,
}

impl From<CriterionSqlRow> for AcceptanceCriterionV1 {
    fn from(row: CriterionSqlRow) -> Self {
        Self {
            key: row.criterion_key,
            description: row.description,
            required: row.required,
            verifier_kind: row.verifier_kind,
            verifier_spec: AcceptanceVerifierSpecV1 {
                path: row.verifier_path,
                command: row.verifier_command,
                pattern: row.verifier_pattern,
                note: row.verifier_note,
            },
        }
    }
}

fn group_criteria_join_rows(rows: Vec<CriteriaJoinRow>) -> Vec<CriteriaGroup> {
    let mut groups: Vec<CriteriaGroup> = Vec::new();
    for row in rows {
        let criterion = match (
            row.criterion_key,
            row.description,
            row.required,
            row.verifier_kind,
        ) {
            (Some(key), Some(description), Some(required), Some(verifier_kind)) => {
                Some(AcceptanceCriterionV1 {
                    key,
                    description,
                    required,
                    verifier_kind,
                    verifier_spec: AcceptanceVerifierSpecV1 {
                        path: row.verifier_path,
                        command: row.verifier_command,
                        pattern: row.verifier_pattern,
                        note: row.verifier_note,
                    },
                })
            }
            _ => None,
        };
        match groups.last_mut() {
            Some(group) if group.memory_id.into_inner() == row.criteria_t => {
                if let Some(criterion) = criterion {
                    group.criteria.push(criterion);
                }
            }
            _ => {
                groups.push(CriteriaGroup {
                    memory_id: MemoryId::new(row.criteria_t),
                    criteria: criterion.into_iter().collect(),
                });
            }
        }
    }
    groups
}

#[allow(clippy::needless_pass_by_value)]
fn map_sqlx(error: sqlx::Error) -> ToolError {
    ToolError::Storage(proxima_core::StorageError::Internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{CriteriaJoinRow, group_criteria_join_rows};
    use crate::payloads::AcceptanceVerifierKind;
    use uuid::Uuid;

    fn join_row(criteria_t: Uuid, key: Option<&str>) -> CriteriaJoinRow {
        CriteriaJoinRow {
            criteria_t,
            criterion_key: key.map(str::to_owned),
            description: key.map(|_| "d".into()),
            required: key.map(|_| true),
            verifier_kind: key.map(|_| AcceptanceVerifierKind::Command),
            verifier_path: None,
            verifier_command: None,
            verifier_pattern: None,
            verifier_note: None,
        }
    }

    #[test]
    fn join_rows_group_by_criteria_t_and_keep_empty_parents() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let groups = group_criteria_join_rows(vec![
            join_row(a, Some("build")),
            join_row(a, Some("tests")),
            join_row(b, None),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].criteria.len(), 2);
        assert_eq!(groups[0].criteria[0].key, "build");
        assert!(groups[1].criteria.is_empty());
    }
}
