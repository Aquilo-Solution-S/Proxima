use proxima_core::storage_ports::{
    FactIngestPort, McpCallReadPort, McpCallWritePort, OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWithCitation, AuthorizedFactWrite, FactIngestOutcome, FactWriteCommand,
};
use proxima_core::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
use proxima_core::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use proxima_core::{SidecarPayload, StorageError};

use crate::error::{internal, with_bounded_retry};
use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl FactIngestPort for PgStorage {
    async fn ingest_fact_atomic(
        &self,
        permit: &OwnerWritePermit,
        draft: &FactWriteCommand,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        verbs::fact_ingest::ingest_fact_atomic(&self.pool, permit, draft, embedding_model_id).await
    }

    async fn ingest_fact_with_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWrite,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        // Retry the whole begin→body→commit on transient deadlock/
        // serialization. The typed sidecar is data (`SidecarPayload`), so each
        // attempt re-clones it and rebuilds the insert closure — unlike an
        // `FnOnce` closure, this is safely re-runnable.
        with_bounded_retry(move || {
            let fact_sidecars = self.sidecars.clone();
            let payload = sidecar_payload.clone();
            async move {
                let mut tx = self.pool.begin().await.map_err(internal)?;
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
        })
        .await
    }

    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        sidecar_payload: &SidecarPayload,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        // Retry the whole begin→body→commit on transient deadlock/
        // serialization; re-clone the citation sidecar payload per attempt.
        with_bounded_retry(move || {
            let sidecars = self.sidecars.clone();
            let fact_sidecars = sidecars.clone();
            let payload = sidecar_payload.clone();
            async move {
                let mut tx = self.pool.begin().await.map_err(internal)?;
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
        })
        .await
    }
}

#[async_trait::async_trait]
impl McpCallWritePort for PgStorage {
    async fn persist_mcp_call_atomic(
        &self,
        permit: &OwnerWritePermit,
        input: &McpCallLogInput,
    ) -> Result<McpCallLogOutcome, StorageError> {
        verbs::persist_mcp_call::persist_mcp_call_atomic(&self.pool, permit, input).await
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
