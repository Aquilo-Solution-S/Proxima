use proxima_core::storage_ports::{
    FactIngestPort, McpCallReadPort, McpCallWritePort, OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWithCitation, AuthorizedFactWrite, FactIngestOutcome, FactWriteCommand,
};
use proxima_core::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
use proxima_core::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use proxima_core::{SidecarPayload, StorageError};

use crate::error::internal;
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
