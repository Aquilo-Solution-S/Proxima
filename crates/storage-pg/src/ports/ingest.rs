use proxima_core::StorageError;
use proxima_core::storage_ports::{FactIngestPort, McpCallReadPort};
use proxima_core::verbs::fact_ingest::{
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, AuthorizedFactWrite,
    FactIngestOutcome,
};
use proxima_core::verbs::mcp_call_history::{McpCallHistoryRequest, McpCallHistoryResponse};

use crate::error::{internal, with_bounded_retry};
use crate::{PgStorage, verbs};

#[async_trait::async_trait]
impl FactIngestPort for PgStorage {
    async fn ingest_authorized_fact_atomic(
        &self,
        authorized: &AuthorizedFactWrite,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        if !authorized.sidecar_payloads().is_empty() {
            return Err(StorageError::ConstraintViolation(
                "typed Fact sidecars require the typed persistence path".into(),
            ));
        }
        verbs::fact_ingest::ingest_authorized_fact_atomic(
            &self.pool,
            authorized,
            embedding_model_id,
        )
        .await
    }

    async fn ingest_fact_with_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWrite,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        let sidecar_payloads = authorized.sidecar_payloads();
        let natural_key = verbs::fact_ingest::fact_natural_key(
            authorized.draft(),
            authorized.fact_sidecar_table(),
            authorized.fact_natural_key_columns(),
            authorized.fact_natural_key_values(),
            sidecar_payloads,
            &self.sidecars,
        )?;
        // Retry the whole begin→body→commit on transient deadlock/
        // serialization. The typed sidecar is data (`SidecarPayload`), so each
        // attempt re-clones it and rebuilds the insert closure — unlike an
        // `FnOnce` closure, this is safely re-runnable.
        with_bounded_retry(move || {
            let natural_key = natural_key.clone();
            let fact_sidecars = self.sidecars.writing(authorized.draft());
            let payloads = sidecar_payloads.to_vec();
            let content_payloads = payloads.clone();
            async move {
                let mut tx = self.pool.begin().await.map_err(internal)?;
                let tables = self.sidecars.tables_for_payloads(&payloads)?;
                let scopes = self.scopes.targets_for_payloads(&payloads)?;
                let outcome = verbs::fact_ingest::ingest_fact_with_sidecar_in_tx(
                    &mut tx,
                    authorized,
                    embedding_model_id,
                    verbs::fact_ingest::FactAdmissionInput {
                        natural_key: natural_key.as_ref(),
                        sidecar_tables: &tables,
                        scopes: &scopes,
                        content: verbs::fact_ingest::ContentResolution {
                            content_id: None,
                            payloads: Some(&content_payloads),
                        },
                        publication: authorized.publication(),
                    },
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
                tx.commit().await.map_err(crate::error::map_err)?;
                Ok(outcome)
            }
        })
        .await
    }

    async fn ingest_fact_with_citation_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitation,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        let sidecar_payloads = authorized.sidecar_payloads();
        let natural_key = verbs::fact_ingest::fact_natural_key(
            authorized.draft(),
            authorized.fact_sidecar_table(),
            authorized.fact_natural_key_columns(),
            authorized.fact_natural_key_values(),
            sidecar_payloads,
            &self.sidecars,
        )?;
        // Retry the whole begin→body→commit on transient deadlock/
        // serialization; re-clone the citation sidecar payload per attempt.
        with_bounded_retry(move || {
            let natural_key = natural_key.clone();
            let sidecars = self.sidecars.clone();
            let fact_sidecars = sidecars.writing(authorized.draft());
            let payloads = sidecar_payloads.to_vec();
            async move {
                let mut tx = self.pool.begin().await.map_err(internal)?;
                let tables = self.sidecars.tables_for_payloads(&payloads)?;
                let scopes = self.scopes.targets_for_payloads(&payloads)?;
                let outcome = verbs::fact_ingest::ingest_fact_with_citation_in_tx(
                    &mut tx,
                    &sidecars,
                    authorized,
                    embedding_model_id,
                    verbs::fact_ingest::FactAdmissionInput {
                        natural_key: natural_key.as_ref(),
                        sidecar_tables: &tables,
                        scopes: &scopes,
                        // A cited Fact resolves its `Content` the same way
                        // the citation routes always have: not at all.
                        content: verbs::fact_ingest::ContentResolution {
                            content_id: None,
                            payloads: None,
                        },
                        publication: authorized.publication(),
                    },
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
                tx.commit().await.map_err(crate::error::map_err)?;
                Ok(outcome)
            }
        })
        .await
    }

    async fn ingest_fact_with_citation_ref_and_typed_sidecar(
        &self,
        authorized: &AuthorizedFactWithCitationRef,
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        let sidecar_payloads = authorized.sidecar_payloads();
        let natural_key = verbs::fact_ingest::fact_natural_key(
            authorized.draft(),
            authorized.fact_sidecar_table(),
            authorized.fact_natural_key_columns(),
            authorized.fact_natural_key_values(),
            sidecar_payloads,
            &self.sidecars,
        )?;
        // Retry the whole begin→body→commit on transient deadlock/
        // serialization; re-clone the sidecar payload per attempt, same as
        // the inline-citation path above.
        with_bounded_retry(move || {
            let natural_key = natural_key.clone();
            let sidecars = self.sidecars.clone();
            let fact_sidecars = sidecars.writing(authorized.draft());
            let payloads = sidecar_payloads.to_vec();
            async move {
                let mut tx = self.pool.begin().await.map_err(internal)?;
                let tables = self.sidecars.tables_for_payloads(&payloads)?;
                let scopes = self.scopes.targets_for_payloads(&payloads)?;
                let outcome = verbs::fact_ingest::ingest_fact_with_citation_ref_in_tx(
                    &mut tx,
                    &sidecars,
                    authorized,
                    embedding_model_id,
                    verbs::fact_ingest::FactAdmissionInput {
                        natural_key: natural_key.as_ref(),
                        sidecar_tables: &tables,
                        scopes: &scopes,
                        // A cited Fact resolves its `Content` the same way
                        // the citation routes always have: not at all.
                        content: verbs::fact_ingest::ContentResolution {
                            content_id: None,
                            payloads: None,
                        },
                        publication: authorized.publication(),
                    },
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
                tx.commit().await.map_err(crate::error::map_err)?;
                Ok(outcome)
            }
        })
        .await
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
