use proxima_core::verbs::query::EntityKind;
use proxima_core::{
    AbstractionPayload, AuthzContext, FactPayload, MemoryId, Owner, SchemaId, ToolError,
};
use proxima_storage_pg::query::{
    ChunkSeriesHead, CodeChunkVectorCandidate, CodeChunkVectorFilters, FileRevisionHeadRow,
    nearest_code_chunk_candidates, owned_chunk_series_heads, owned_file_revision_heads,
    readable_chunk_head_ts_for_file, readable_file_revision_head_ts,
};
use sqlx::PgPool;

/// Private code-flavor storage service passed to tools by the host.
///
/// Authz-filtered payload reads delegate to `proxima::flavor` (`&Engine`).
/// Code-series head / ANN helpers call `proxima_storage_pg::query` here —
/// they need the flavor's private pool and must not sit on the Flavor SDK.
/// `pool()` stays private (`from_backend_pool_for_host`/`for_tests`).
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

    /// Owner-only current file-revision heads of `repo_id`. Head is
    /// `memory_head`; ingest compares these shas against git.
    pub(crate) async fn owned_file_revision_heads(
        &self,
        owner: Owner,
        repo_id: uuid::Uuid,
    ) -> Result<Vec<FileRevisionHeadRow>, ToolError> {
        owned_file_revision_heads(
            &self.pool,
            owner,
            &crate::payloads::FileRevisionV1::schema_id(),
            repo_id,
        )
        .await
        .map_err(ToolError::Storage)
    }

    /// Owner∪World current file-revision `t`s for one path.
    pub(crate) async fn readable_file_revision_head_ts(
        &self,
        owner: Owner,
        repo_id: uuid::Uuid,
        file_path: &str,
    ) -> Result<Vec<uuid::Uuid>, ToolError> {
        readable_file_revision_head_ts(
            &self.pool,
            owner,
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

    /// Owner∪World present chunk head `t`s for one file.
    pub(crate) async fn readable_chunk_head_ts_for_file(
        &self,
        owner: Owner,
        repo_id: uuid::Uuid,
        file_path: &str,
    ) -> Result<Vec<uuid::Uuid>, ToolError> {
        readable_chunk_head_ts_for_file(
            &self.pool,
            owner,
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
            owner,
            &crate::payloads::CodeChunkV1::schema_id(),
            model_id,
            query_embedding,
            filters,
            i64::try_from(limit).unwrap_or(i64::MAX),
        )
        .await
        .map_err(ToolError::Storage)
    }
}
