//! Backend-owned [`UnitOfWork`]: one transaction, several Engine writes.

use super::Engine;
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::edge::EdgeEndpoint;
use crate::error::ProtocolError;
use crate::storage::{AuthorDerivedRequest, DerivedEmbedding};
use crate::storage_ports::WriteSession;
use crate::verbs::fact_ingest::{FactIngestOutcome, FactWriteCommand};
use crate::{
    AuthorDerivedAuthorizedOutcome, AuthorDerivedRequestInput, FactPayload, MemoryId, Owner,
    SidecarPayload, SourceBatchId,
};

/// One transaction the Engine can attach several authorized writes to.
/// Drop without [`Self::commit`] rolls the transaction back.
pub struct UnitOfWork<'a> {
    engine: &'a Engine,
    authz: &'a AuthzContext,
    session: Option<Box<dyn WriteSession>>,
}

impl std::fmt::Debug for UnitOfWork<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnitOfWork")
            .field("committed", &self.session.is_none())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Open a backend-owned write transaction.
    ///
    /// # Errors
    ///
    /// Storage faults from beginning the transaction.
    pub async fn unit_of_work<'a>(
        &'a self,
        authz: &'a AuthzContext,
    ) -> Result<UnitOfWork<'a>, ProtocolError> {
        let session = self
            .storage()
            .write_session
            .begin()
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))?;
        Ok(UnitOfWork {
            engine: self,
            authz,
            session: Some(session),
        })
    }

    /// One-shot Fact + typed sidecar: [`UnitOfWork`] of one, then commit.
    ///
    /// # Errors
    ///
    /// Authorization, schema, or storage faults from the ingest.
    pub async fn ingest_typed_fact<P>(
        &self,
        authz: &AuthzContext,
        source_id: &str,
        payload: &P,
    ) -> Result<FactIngestOutcome, ProtocolError>
    where
        P: FactPayload + Clone,
    {
        let mut uow = self.unit_of_work(authz).await?;
        let outcome = uow.ingest_fact(source_id, payload).await?;
        uow.commit().await?;
        Ok(outcome)
    }
}

impl UnitOfWork<'_> {
    fn session_mut(&mut self) -> Result<&mut Box<dyn WriteSession>, ProtocolError> {
        self.session
            .as_mut()
            .ok_or_else(|| ProtocolError::internal("unit of work already committed"))
    }

    /// Serialize this transaction against `key` (`pg_advisory_xact_lock`).
    ///
    /// # Errors
    ///
    /// Storage faults from the lock.
    pub async fn advisory_xact_lock(&mut self, key: i64) -> Result<(), ProtocolError> {
        self.session_mut()?
            .advisory_xact_lock(key)
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))
    }

    /// Authorize and persist one typed Fact + sidecar in this transaction.
    ///
    /// # Errors
    ///
    /// Authorization, schema, or storage faults.
    pub async fn ingest_fact<P>(
        &mut self,
        source_id: &str,
        payload: &P,
    ) -> Result<FactIngestOutcome, ProtocolError>
    where
        P: FactPayload + Clone,
    {
        let observed_at = time::OffsetDateTime::now_utc();
        let draft = FactWriteCommand::from_payload(
            source_id,
            SourceBatchId::new(uuid::Uuid::now_v7()),
            payload,
            observed_at,
        );
        let sidecars = [SidecarPayload::fact(payload.clone())];
        let authorized = self
            .engine
            .authorize_fact_ingest(self.authz, Relation::Editor, draft, &sidecars)
            .await?;
        self.engine
            .validate_write_permit(authorized.owner_write_permit())?;
        let embed_client = self.engine.embed_client();
        let requested = embed_client.as_ref().map(|client| client.model_id());
        let embedding_model_id = self
            .engine
            .vector_model_for(authorized.draft().schema_id.as_str(), requested);
        self.session_mut()?
            .ingest_fact_with_typed_sidecar(&authorized, &sidecars, embedding_model_id)
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(
                    err,
                    "fact",
                    "fact ingest referenced row not found",
                )
            })
    }

    /// Authorize and persist one derived memory in this transaction.
    ///
    /// # Errors
    ///
    /// Authorization or storage faults.
    pub async fn author_derived(
        &mut self,
        req: AuthorDerivedRequestInput<'_>,
    ) -> Result<AuthorDerivedAuthorizedOutcome, ProtocolError> {
        let write_permit = self
            .engine
            .authorize_write(self.authz, &req.owner, Relation::Editor)
            .await?;
        let owner = *write_permit.owner();
        if let Some(prior) = req.supersedes {
            let prior_home = self
                .engine
                .storage()
                .memory_authoring
                .owner_access_read
                .home_owner(crate::EntityId::Memory(prior))
                .await
                .map_err(|err| ProtocolError::internal(err.to_string()))?;
            if prior_home.as_ref() != Some(&owner) {
                return Err(ProtocolError::forbidden(
                    "supersedes target is not an owned entity of the same owner",
                ));
            }
            let prior_kind = self.engine.load_required_memory_kind(&owner, prior).await?;
            if prior_kind != req.kind {
                return Err(ProtocolError::invalid_argument(
                    "supersedes",
                    "must supersede a memory of the same kind",
                ));
            }
        }
        let source = EdgeEndpoint::memory(req.kind, req.memory_id);
        let origins = self
            .engine
            .authorized_index_targets(self.authz, source, req.derived_from, "derived_from")
            .await?;
        let declared = req.sidecar_payload.references();
        let references = self
            .engine
            .authorized_payload_references(self.authz, source, &declared)
            .await?;
        super::memory_authoring::validate_operator_memory_invocation_request(&req)
            .map_err(super::memory_authoring::map_derived_storage_error)?;
        let client = self.engine.embed_client();
        let embedding = match client.as_deref() {
            None => DerivedEmbedding::None,
            Some(client) => {
                super::memory_authoring::resolve_derived_embedding(client, req.memory_id, &req.text)
                    .await
                    .map_err(super::memory_authoring::map_derived_storage_error)?
            }
        };
        let storage_req = AuthorDerivedRequest {
            memory_id: req.memory_id,
            owner,
            kind: req.kind,
            text: req.text,
            schema_id: req.schema_id,
            schema_version: req.schema_version,
            operator_kind: req.operator_kind,
            operator_id: req.operator_id,
            input_contract_id: req.input_contract_id,
            source_batch_id: None,
            model_id: req.model_id,
            prompt_version: req.prompt_version,
            sidecar_payload: req.sidecar_payload,
            authoring_perspective_id: req.authoring_perspective_id,
            supersedes: req.supersedes,
            lexical_language: req.lexical_language,
            embedding,
            origins: &origins,
            references: &references,
        };
        let outcome = self
            .session_mut()?
            .author_derived(&storage_req, write_permit.owner_write_permit())
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))?;
        Ok(AuthorDerivedAuthorizedOutcome {
            memory_id: outcome.memory_id,
            idempotent_replay: outcome.idempotent_replay,
            edge_count: outcome.edge_count,
            embedding_deferred: outcome.embedding_deferred,
        })
    }

    /// Cool one owned memory `t` in this transaction.
    ///
    /// # Errors
    ///
    /// Authorization or storage faults.
    pub async fn forget(&mut self, owner: Owner, memory_id: MemoryId) -> Result<(), ProtocolError> {
        let write_permit = self
            .engine
            .authorize_write(self.authz, &owner, Relation::Editor)
            .await?;
        self.session_mut()?
            .forget_memory(write_permit.owner_write_permit(), memory_id)
            .await
            .map_err(|err| {
                super::errors::map_write_storage_error(err, "memory", "memory not found")
            })
    }

    /// Commit the transaction. Further methods fail.
    ///
    /// # Errors
    ///
    /// Storage commit faults.
    pub async fn commit(mut self) -> Result<(), ProtocolError> {
        let session = self
            .session
            .take()
            .ok_or_else(|| ProtocolError::internal("unit of work already committed"))?;
        session
            .commit()
            .await
            .map_err(|err| ProtocolError::internal(err.to_string()))
    }
}

impl Drop for UnitOfWork<'_> {
    fn drop(&mut self) {
        self.session.take();
    }
}
