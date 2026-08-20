use std::sync::Arc;

use proxima_core::storage_ports::{OwnerWritePermit, WriteSession, WriteSessionFactory};
use proxima_core::verbs::fact_ingest::{AuthorizedFactWrite, FactIngestOutcome};
use proxima_core::verbs::goal_write::{CreateGoalAtomicRequest, GoalWriteOutcome};
use proxima_core::{
    AuthorDerivedOutcome, AuthorDerivedRequest, ColdObjectStore, MemoryId, SidecarPayload,
    StorageError, cold_object_key,
};
use sqlx::{Postgres, Transaction};

use crate::error::internal;
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::{PgStorage, verbs};

struct PgWriteSession {
    tx: Transaction<'static, Postgres>,
    sidecars: PgSidecarRegistryFrozen,
    cold: Arc<dyn ColdObjectStore>,
}

#[async_trait::async_trait]
impl WriteSessionFactory for PgStorage {
    async fn begin(&self) -> Result<Box<dyn WriteSession>, StorageError> {
        let tx = self.pool.begin().await.map_err(internal)?;
        Ok(Box::new(PgWriteSession {
            tx,
            sidecars: self.sidecars.clone(),
            cold: Arc::clone(&self.cold),
        }))
    }
}

#[async_trait::async_trait]
impl WriteSession for PgWriteSession {
    async fn advisory_xact_lock(&mut self, key: i64) -> Result<(), StorageError> {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(key)
            .execute(&mut *self.tx)
            .await
            .map_err(crate::error::map_err)?;
        Ok(())
    }

    async fn ingest_fact_with_typed_sidecar(
        &mut self,
        authorized: &AuthorizedFactWrite,
        sidecar_payloads: &[SidecarPayload],
        embedding_model_id: Option<&str>,
    ) -> Result<FactIngestOutcome, StorageError> {
        let fact_sidecars = self.sidecars.clone();
        let payloads = sidecar_payloads.to_vec();
        let tables = self.sidecars.tables_for_payloads(sidecar_payloads)?;
        let owner = authorized.owner_write_permit().owner();
        let owner_id =
            crate::access::owner_columns::ensure_owner_row(self.tx.as_mut(), owner).await?;
        let content_id = verbs::content::ensure_content_from_payloads(
            &mut self.tx,
            owner_id,
            authorized.draft().schema_id.as_str(),
            sidecar_payloads,
        )
        .await?;
        let outcome = verbs::fact_ingest::ingest_fact_with_sidecar_in_tx(
            &mut self.tx,
            authorized,
            embedding_model_id,
            &tables,
            content_id,
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
        if !outcome.idempotent_replay {
            verbs::sketch::upsert_sketch(
                &mut self.tx,
                authorized.owner_write_permit().owner().stored_owner_id(),
                outcome.memory_id.into_inner(),
                authorized.draft().kind.as_str(),
                &verbs::sketch::sketch_line(
                    authorized.draft().kind.as_str(),
                    authorized.draft().rendered_text.as_deref(),
                    sidecar_payloads,
                ),
            )
            .await?;
        }
        Ok(outcome)
    }

    async fn author_derived(
        &mut self,
        req: &AuthorDerivedRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<AuthorDerivedOutcome, StorageError> {
        let draft = verbs::derive_append::DerivedDraft {
            memory_id: req.memory_id.into_inner(),
            owner: req.owner,
            kind: req.kind,
            schema_id: req.schema_id.clone(),
            schema_version: req.schema_version,
            text: req.text.clone(),
            operator_kind: req.operator_kind,
            model_id: req.model_id,
            supersedes: req.supersedes,
            lexical_language: req.lexical_language,
            embedding: req.embedding.clone(),
        };
        verbs::derive_append::validate_derived_origins_in_tx(&mut self.tx, &draft, req.origins)
            .await?;
        verbs::derive_append::validate_derived_reference_kinds_in_tx(&mut self.tx, req.references)
            .await?;
        let sidecars = self.sidecars.clone();
        let sidecar_payload = req.sidecar_payload.clone();
        let tables = self
            .sidecars
            .tables_for_payloads(std::slice::from_ref(&sidecar_payload))?;
        let owner_id =
            crate::access::owner_columns::ensure_owner_row(self.tx.as_mut(), permit.owner())
                .await?;
        let content_id = verbs::content::ensure_content_from_payloads(
            &mut self.tx,
            owner_id,
            req.schema_id.as_str(),
            std::slice::from_ref(&sidecar_payload),
        )
        .await?;
        let outcome = verbs::derive_append::append_derived_in_tx(
            &mut self.tx,
            permit,
            &draft,
            req.origins,
            req.references,
            &tables,
            content_id,
            move |tx, outcome| {
                Box::pin(async move {
                    sidecars
                        .insert_memory_sidecar(tx, outcome.memory_id, &sidecar_payload)
                        .await
                })
            },
        )
        .await?;
        if !outcome.idempotent_replay {
            let kind = match req.kind {
                proxima_core::EntityKind::Fact => "fact",
                proxima_core::EntityKind::Abstraction => "abstraction",
                proxima_core::EntityKind::Perspective => "perspective",
                proxima_core::EntityKind::Goal => "goal",
            };
            verbs::sketch::upsert_sketch(
                &mut self.tx,
                owner_id,
                outcome.memory_id.into_inner(),
                kind,
                &verbs::sketch::sketch_line(
                    kind,
                    Some(req.text.as_str()),
                    std::slice::from_ref(&req.sidecar_payload),
                ),
            )
            .await?;
        }
        let edge_count = verbs::derive_append::assert_derived_index_rows(
            &mut self.tx,
            &draft,
            &outcome,
            req.origins,
            req.references,
        )
        .await?;
        Ok(AuthorDerivedOutcome {
            memory_id: outcome.memory_id,
            idempotent_replay: outcome.idempotent_replay,
            edge_count,
            embedding_deferred: req.embedding.is_deferred() && !outcome.idempotent_replay,
        })
    }

    async fn create_goal(
        &mut self,
        req: &CreateGoalAtomicRequest<'_>,
        permit: &OwnerWritePermit,
    ) -> Result<GoalWriteOutcome, StorageError> {
        verbs::goal_write::create_goal_in_tx(&mut self.tx, &self.sidecars, req, permit).await
    }

    async fn forget_memory(
        &mut self,
        permit: &OwnerWritePermit,
        memory_id: MemoryId,
    ) -> Result<(), StorageError> {
        let owner = permit.owner();
        let owner_id = owner.stored_owner_id();
        let t = memory_id.into_inner();
        // Ownership precondition, not a key ingredient: the cold key is
        // `cold/<t>` now, but a `t` the caller does not own must still be
        // NotFound rather than a forget on someone else's row.
        sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT handle FROM proxima_core.memory WHERE t = $1 AND owner_id = $2",
        )
        .bind(t)
        .bind(owner_id)
        .fetch_optional(&mut *self.tx)
        .await
        .map_err(internal)?
        .ok_or(StorageError::NotFound)?;
        let key = cold_object_key(t);
        verbs::forget::forget_memory(
            &mut self.tx,
            &self.sidecars,
            self.cold.as_ref(),
            &key,
            t,
            owner_id,
        )
        .await
    }

    async fn commit(self: Box<Self>) -> Result<(), StorageError> {
        self.tx.commit().await.map_err(crate::error::map_err)
    }
}
