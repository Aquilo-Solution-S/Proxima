use proxima_core::storage::FactProvenanceSpec;
use proxima_core::storage_ports::{
    FactIngestPort, McpCallReadPort, McpCallWritePort, OwnerWritePermit,
};
use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, AuthorizedFactWrite,
    FactIngestOutcome, FactWriteCommand,
};
use proxima_core::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};
use proxima_core::verbs::persist_mcp_call::{McpCallLogInput, McpCallLogOutcome};
use proxima_core::{EdgeAuthorshipKind, EntityKind, Owner, SidecarPayload, StorageError};

use crate::error::{internal, with_bounded_retry};
use crate::{PgStorage, verbs};

/// Write the Fact's declared provenance edge in the Fact's own transaction.
///
/// Same transaction, because a Fact is append-only: a provenance write
/// that crashed after the Fact landed would never be repaired — the retry
/// replays on the receipt and reports success.
///
/// Content-derived id, because a retry would otherwise append a SECOND
/// edge rather than converge (measured: two rows from one retry). On a
/// replay `outcome.memory_id` is the Fact already stored, so the insert
/// lands on the existing row and no-ops.
async fn write_fact_provenance_in_tx(
    tx: &mut sqlx::PgConnection,
    owner: Owner,
    fact_memory_id: uuid::Uuid,
    provenance: Option<FactProvenanceSpec<'_>>,
) -> Result<(), StorageError> {
    let Some(provenance) = provenance else {
        return Ok(());
    };
    let relation = provenance.relation.descriptor.relation.as_str();
    let draft = verbs::edge_append::EdgeDraft {
        edge_id: verbs::edge_append::content_addressed_edge_id(
            owner,
            relation,
            fact_memory_id,
            provenance.target_memory_id.into_inner(),
            EdgeAuthorshipKind::SourceIngest,
        ),
        relation: provenance.relation,
        source_kind: EntityKind::Fact,
        source_memory_id: Some(fact_memory_id),
        source_goal_id: None,
        source_fact_entity_id: None,
        target_kind: provenance.target_kind,
        target_memory_id: Some(provenance.target_memory_id.into_inner()),
        target_goal_id: None,
        target_fact_entity_id: None,
        authorship_kind: EdgeAuthorshipKind::SourceIngest,
        authorship_owner_memory_id: None,
        owner: &owner,
    };
    verbs::edge_append::append_edge_in_tx(tx, &draft).await
}

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
        sidecar_payloads: &[SidecarPayload],
        embedding_model_id: Option<&str>,
        provenance: Option<FactProvenanceSpec<'_>>,
    ) -> Result<FactIngestOutcome, StorageError> {
        // Retry the whole begin→body→commit on transient deadlock/
        // serialization. The typed sidecar is data (`SidecarPayload`), so each
        // attempt re-clones it and rebuilds the insert closure — unlike an
        // `FnOnce` closure, this is safely re-runnable.
        with_bounded_retry(move || {
            let fact_sidecars = self.sidecars.clone();
            let payloads = sidecar_payloads.to_vec();
            async move {
                let mut tx = self.pool.begin().await.map_err(internal)?;
                let outcome = verbs::fact_ingest::ingest_fact_with_sidecar_in_tx(
                    &mut tx,
                    authorized,
                    embedding_model_id,
                    move |tx, outcome| {
                        Box::pin(async move {
                            for payload in &payloads {
                                fact_sidecars
                                    .insert_memory_sidecar(tx, outcome.memory_id, payload)
                                    .await?;
                            }

                            Ok(())
                        })
                    },
                )
                .await?;
                write_fact_provenance_in_tx(
                    tx.as_mut(),
                    *authorized.permit().owner(),
                    outcome.memory_id.into_inner(),
                    provenance,
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
        sidecar_payloads: &[SidecarPayload],
        embedding_model_id: Option<&str>,
        provenance: Option<FactProvenanceSpec<'_>>,
    ) -> Result<FactIngestOutcome, StorageError> {
        // Retry the whole begin→body→commit on transient deadlock/
        // serialization; re-clone the citation sidecar payload per attempt.
        with_bounded_retry(move || {
            let sidecars = self.sidecars.clone();
            let fact_sidecars = sidecars.clone();
            let payloads = sidecar_payloads.to_vec();
            async move {
                let mut tx = self.pool.begin().await.map_err(internal)?;
                let outcome = verbs::fact_ingest::ingest_fact_with_citation_in_tx(
                    &mut tx,
                    &sidecars,
                    authorized,
                    embedding_model_id,
                    move |tx, outcome| {
                        Box::pin(async move {
                            for payload in &payloads {
                                fact_sidecars
                                    .insert_memory_sidecar(tx, outcome.memory_id, payload)
                                    .await?;
                            }

                            Ok(())
                        })
                    },
                )
                .await?;
                write_fact_provenance_in_tx(
                    tx.as_mut(),
                    *authorized.permit().owner(),
                    outcome.memory_id.into_inner(),
                    provenance,
                )
                .await?;
                tx.commit().await.map_err(crate::error::map_err)?;
                Ok(outcome)
            }
        })
        .await
    }

    async fn ingest_fact_with_citation_ref_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitationRef,
        sidecar_payloads: &[SidecarPayload],
        embedding_model_id: Option<&str>,
        provenance: Option<FactProvenanceSpec<'_>>,
    ) -> Result<FactIngestOutcome, StorageError> {
        // Retry the whole begin→body→commit on transient deadlock/
        // serialization; re-clone the sidecar payload per attempt, same as
        // the inline-citation path above.
        with_bounded_retry(move || {
            let sidecars = self.sidecars.clone();
            let fact_sidecars = sidecars.clone();
            let payloads = sidecar_payloads.to_vec();
            async move {
                let mut tx = self.pool.begin().await.map_err(internal)?;
                let outcome = verbs::fact_ingest::ingest_fact_with_citation_ref_in_tx(
                    &mut tx,
                    &sidecars,
                    authorized,
                    embedding_model_id,
                    move |tx, outcome| {
                        Box::pin(async move {
                            for payload in &payloads {
                                fact_sidecars
                                    .insert_memory_sidecar(tx, outcome.memory_id, payload)
                                    .await?;
                            }

                            Ok(())
                        })
                    },
                )
                .await?;
                write_fact_provenance_in_tx(
                    tx.as_mut(),
                    *authorized.permit().owner(),
                    outcome.memory_id.into_inner(),
                    provenance,
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
