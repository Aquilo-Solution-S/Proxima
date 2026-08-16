use proxima_core::verbs::query::EntityKind;
use proxima_core::{
    AbstractionPayload, AuthzContext, FactPayload, MemoryId, Owner, SchemaId, ToolError,
};
use sqlx::PgPool;

/// Private code-flavor storage service passed to tools by the host.
///
/// All authorized-read logic lives in `proxima::flavor`;
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

    /// Narrow a sidecar-only `code-chunk-v1` candidate id list to the
    /// current `memory_head` `t` of each series (owner-or-World). Ingest
    /// owns one handle per `(owner, repo, path, index)`.
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

    /// Owner-only current file-revision heads of `repo_id`. Head is
    /// `memory_head`; ingest compares these shas against git.
    pub(crate) async fn owned_file_revision_heads(
        &self,
        owner: Owner,
        repo_id: uuid::Uuid,
    ) -> Result<Vec<proxima::flavor::FileRevisionHeadRow>, ToolError> {
        proxima::flavor::owned_file_revision_heads(
            &self.pool,
            owner,
            &crate::payloads::FileRevisionV1::schema_id(),
            repo_id,
        )
        .await
    }

    /// Owner∪World current file-revision `t`s for one path.
    pub(crate) async fn readable_file_revision_head_ts(
        &self,
        owner: Owner,
        repo_id: uuid::Uuid,
        file_path: &str,
    ) -> Result<Vec<uuid::Uuid>, ToolError> {
        proxima::flavor::readable_file_revision_head_ts(
            &self.pool,
            owner,
            &crate::payloads::FileRevisionV1::schema_id(),
            repo_id,
            file_path,
        )
        .await
    }

    /// Owner-only present chunk indexes at current heads of one file.
    pub(crate) async fn owned_present_chunk_indexes(
        &self,
        owner: Owner,
        repo_id: uuid::Uuid,
        file_path: &str,
    ) -> Result<Vec<i32>, ToolError> {
        proxima::flavor::owned_present_chunk_indexes(
            &self.pool,
            owner,
            &crate::payloads::CodeChunkV1::schema_id(),
            repo_id,
            file_path,
        )
        .await
    }

    /// Owner∪World present chunk head `t`s for one file.
    pub(crate) async fn readable_chunk_head_ts_for_file(
        &self,
        owner: Owner,
        repo_id: uuid::Uuid,
        file_path: &str,
    ) -> Result<Vec<uuid::Uuid>, ToolError> {
        proxima::flavor::readable_chunk_head_ts_for_file(
            &self.pool,
            owner,
            &crate::payloads::CodeChunkV1::schema_id(),
            repo_id,
            file_path,
        )
        .await
    }

    /// Nearest `code-chunk-v1` chunks to a query embedding, best-first.
    ///
    /// A candidate producer like
    /// [`Self::authorized_code_chunk_head_candidates`]: what it returns is
    /// still narrowed and authorized by that call and then by
    /// [`Self::authorized_abstraction_payloads`]. The embeddings it ranks
    /// against live in `proxima_core.embeddings`, which flavor SQL may not
    /// join, so the query itself is backend-owned.
    pub(crate) async fn nearest_code_chunk_candidates(
        &self,
        owner: Owner,
        model_id: &str,
        query_embedding: &[f32],
        filters: proxima::flavor::CodeChunkVectorFilters<'_>,
        limit: usize,
    ) -> Result<Vec<proxima::flavor::CodeChunkVectorCandidate>, ToolError> {
        proxima::flavor::nearest_code_chunk_candidates(
            &self.pool,
            owner,
            &crate::payloads::CodeChunkV1::schema_id(),
            model_id,
            query_embedding,
            filters,
            limit,
        )
        .await
    }
}
