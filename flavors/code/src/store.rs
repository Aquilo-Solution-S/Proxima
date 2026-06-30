use std::collections::{HashMap, HashSet};

use proxima_core::verbs::query::{EntityKind, QueryRequest, SupersessionStatus, TombstoneFilter};
use proxima_core::{
    AbstractionPayload, AuthzContext, FactPayload, MemoryId, Owner, SchemaId, SidecarPayload,
    ToolError,
};
use sqlx::PgPool;

const MAX_AUTHZ_CANDIDATES: usize = 2_000;

/// Private code-flavor storage service passed to tools by the host.
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
        let candidates = bounded_candidates(candidates, limit);
        if candidates.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let mut req = QueryRequest::for_principal(owner);
        req.entity_kind = Some(entity_kind);
        req.schema_id = schema_id;
        req.supersession = SupersessionStatus::HeadsOnly;
        req.tombstones = TombstoneFilter::PresentOnly;
        req.limit = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
        req.include_payloads = false;
        req.memory_ids = candidates.iter().copied().map(MemoryId::new).collect();

        let visible = engine.query(authz, &req).await?;
        let visible_ids = visible
            .memories
            .into_iter()
            .map(|row| row.id.into_inner())
            .collect::<HashSet<_>>();

        Ok(candidates
            .into_iter()
            .filter(|id| visible_ids.contains(id))
            .take(limit)
            .map(MemoryId::new)
            .collect())
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
        let payloads = self
            .authorized_payloads(
                engine,
                authz,
                owner,
                candidates,
                EntityKind::Fact,
                P::schema_id(),
                SupersessionStatus::HeadsOnly,
                limit,
            )
            .await?;
        Ok(payloads
            .into_iter()
            .filter_map(|(id, payload)| payload.downcast_ref::<P>().cloned().map(|p| (id, p)))
            .collect())
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
        let payloads = self
            .authorized_payloads(
                engine,
                authz,
                owner,
                candidates,
                EntityKind::Abstraction,
                P::schema_id(),
                SupersessionStatus::HeadsOnly,
                limit,
            )
            .await?;
        Ok(payloads
            .into_iter()
            .filter_map(|(id, payload)| payload.downcast_ref::<P>().cloned().map(|p| (id, p)))
            .collect())
    }

    pub(crate) async fn authorized_abstraction_payloads_include_superseded<P>(
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
        let payloads = self
            .authorized_payloads(
                engine,
                authz,
                owner,
                candidates,
                EntityKind::Abstraction,
                P::schema_id(),
                SupersessionStatus::IncludeSuperseded,
                limit,
            )
            .await?;
        Ok(payloads
            .into_iter()
            .filter_map(|(id, payload)| payload.downcast_ref::<P>().cloned().map(|p| (id, p)))
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    async fn authorized_payloads(
        &self,
        engine: &proxima_core::Engine,
        authz: &AuthzContext,
        owner: Owner,
        candidates: &[uuid::Uuid],
        entity_kind: EntityKind,
        schema_id: SchemaId,
        supersession: SupersessionStatus,
        limit: usize,
    ) -> Result<Vec<(MemoryId, SidecarPayload)>, ToolError> {
        let candidates = bounded_candidates(candidates, limit);
        if candidates.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let mut req = QueryRequest::for_principal(owner);
        req.entity_kind = Some(entity_kind);
        req.schema_id = Some(schema_id);
        req.supersession = supersession;
        req.tombstones = TombstoneFilter::PresentOnly;
        req.limit = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
        req.include_payloads = true;
        req.memory_ids = candidates.iter().copied().map(MemoryId::new).collect();

        let response = engine.query(authz, &req).await?;
        let mut by_id = response
            .memories
            .into_iter()
            .filter_map(|row| row.payload.map(|payload| (row.id.into_inner(), payload)))
            .collect::<HashMap<_, _>>();

        Ok(candidates
            .into_iter()
            .filter_map(|id| {
                by_id
                    .remove(&id)
                    .map(|payload| (MemoryId::new(id), payload))
            })
            .take(limit)
            .collect())
    }
}

fn bounded_candidates(candidates: &[uuid::Uuid], _limit: usize) -> Vec<uuid::Uuid> {
    let cap = candidates.len().min(MAX_AUTHZ_CANDIDATES);
    let mut seen = HashSet::with_capacity(cap);
    candidates
        .iter()
        .copied()
        .filter(|id| seen.insert(*id))
        .take(MAX_AUTHZ_CANDIDATES)
        .collect()
}
