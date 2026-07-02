use proxima_core::verbs::query::EntityKind;
use proxima_core::{
    AbstractionPayload, AuthzContext, FactPayload, MemoryId, Owner, SchemaId, ToolError,
};
use sqlx::PgPool;

/// Private code-flavor storage service passed to tools by the host.
///
/// All authorized-read logic lives in `proxima::flavor` (v0.0.5 Task 5);
/// the methods here are thin delegating wrappers so call sites across this
/// crate keep a stable `pool.authorized_*(...)` shape while `pool()` itself
/// stays private — no `PgPool` and no `proxima_core.*` SQL ever leaves this
/// crate's backend-owned boundary (`from_backend_pool_for_host`/`for_tests`,
/// `pool()`).
#[derive(Clone)]
pub struct CodeFlavorStore {
    pool: PgPool,
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
    pub fn from_backend_pool_for_host(pool: PgPool) -> Self {
        Self { pool }
    }

    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    #[must_use]
    pub fn from_backend_pool_for_tests(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
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

    pub(crate) async fn authorized_fact_payloads_include_tombstones<P>(
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
        proxima::flavor::authorized_fact_payloads_include_tombstones::<P>(
            engine, authz, owner, candidates, limit,
        )
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

    /// Narrow a sidecar-only `code-chunk-v1` candidate id list (already
    /// known to belong to that schema via a `proxima_code.*`-only query) to
    /// the subset not superseded, by `(repo_id, file_path, chunk_index)`,
    /// within the same schema/owner-or-World scope. `code-chunk-v1` never
    /// sets `memories.supersedes`; see
    /// `proxima::flavor::authorized_code_chunk_head_candidates`.
    pub(crate) async fn authorized_code_chunk_head_candidates(
        &self,
        owner: Owner,
        candidates: &[uuid::Uuid],
    ) -> Result<Vec<uuid::Uuid>, ToolError> {
        proxima::flavor::authorized_code_chunk_head_candidates(
            &self.pool,
            owner,
            &crate::payloads::CodeChunkV1::schema_id(),
            candidates,
        )
        .await
    }
}
