//! Owner routes, durable embedding work, and host maintenance.

use super::Engine;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::llm::{
    BoundEmbeddingClient, ChunkedEmbedding, EmbeddingConfigurationFault, EmbeddingInputRefusal,
    EmbeddingRoute, EmbeddingRouteError, EmbeddingSpace, LlmError,
};
use crate::storage::{EmbeddingJobClaim, StorageError};
use crate::storage_ports::{EmbeddingJobHandle, EmbeddingWrite};
use crate::{EmbeddableEntityRef, EntityKind, MemoryId, OwnerId};

mod control;
pub(crate) use control::EmbeddingControl;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmbeddingDrainOutcome {
    pub processed: usize,
    pub failed: usize,
    /// Active observed client faults, sorted and deduplicated for health.
    pub configuration_faults: Vec<EmbeddingConfigurationFault>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmbedStep {
    Embedded,
    NothingToEmbed,
}

struct EmbeddingClaimHeartbeat {
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for EmbeddingClaimHeartbeat {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn spawn_embedding_claim_heartbeat(
    jobs: EmbeddingJobHandle,
    claims: Vec<EmbeddingJobClaim>,
    interval: std::time::Duration,
) -> EmbeddingClaimHeartbeat {
    let handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            if let Err(err) = jobs.renew_embedding_jobs(&claims).await {
                tracing::warn!(error = %err, "embedding claim heartbeat failed");
            }
        }
    });
    EmbeddingClaimHeartbeat { handle }
}

/// Group `items` by `key`, keeping first-seen order of keys and items.
fn group_claims<T, K: PartialEq>(items: Vec<T>, key: impl Fn(&T) -> K) -> Vec<(K, Vec<T>)> {
    let mut groups: Vec<(K, Vec<T>)> = Vec::new();
    for item in items {
        let item_key = key(&item);
        match groups.iter_mut().find(|(group, _)| *group == item_key) {
            Some((_, members)) => members.push(item),
            None => groups.push((item_key, vec![item])),
        }
    }
    groups
}

impl Engine {
    /// The route for memories `owner` owns, every client behind the host's
    /// request deadline. No router means [`EmbeddingRoute::none`].
    ///
    /// `owner` is the Owner of the data being written or searched, never
    /// the caller: the engine embeds an Owner's texts and queries only
    /// through this route.
    ///
    /// # Errors
    ///
    /// The router's [`EmbeddingRouteError`]; callers fail closed on it.
    pub async fn embedding_route(
        &self,
        owner: &OwnerId,
    ) -> Result<EmbeddingRoute, EmbeddingRouteError> {
        let Some(router) = &self.embedding_router else {
            return Ok(EmbeddingRoute::none());
        };
        let route = router.route(owner).await?;
        Ok(self
            .embedding_control
            .observe(*owner, route, self.embedding_runtime_policy))
    }

    /// Configuration faults on currently observed bindings. This snapshot
    /// never calls a provider or waits for an in-flight embedding request.
    /// Route changes are observed by ordinary route lookup and the drain.
    #[must_use]
    pub fn embedding_configuration_faults(&self) -> Vec<EmbeddingConfigurationFault> {
        self.embedding_control.configuration_faults()
    }

    async fn refresh_blocked_embedding_routes(&self) {
        let now = tokio::time::Instant::now();
        for owner in self.embedding_control.owners_to_refresh(now) {
            if let Err(err) = self.embedding_route(&owner).await {
                self.embedding_control.route_failed(
                    owner,
                    self.embedding_runtime_policy.worker_interval(),
                    tokio::time::Instant::now(),
                );
                tracing::warn!(%owner, error = %err, "blocked embedding owner route could not be refreshed");
            }
        }
    }

    /// `owner`'s route for a write: a route the host refuses refuses the
    /// write. The host's message can name its own configuration, so it
    /// stays in the log.
    pub(in crate::engine) async fn write_route(
        &self,
        owner: &OwnerId,
    ) -> Result<EmbeddingRoute, ProtocolError> {
        self.embedding_route(owner).await.map_err(|err| {
            tracing::warn!(
                owner = %owner.to_string(),
                error = %err,
                "embedding route refused"
            );
            ProtocolError::internal("no embedding route for this owner")
        })
    }

    /// `owner`'s route for a search: a route the host refuses reads as
    /// [`EmbeddingRoute::none`], so the search runs without a semantic arm
    /// instead of failing.
    pub async fn search_route(&self, owner: &OwnerId) -> EmbeddingRoute {
        self.embedding_route(owner).await.unwrap_or_else(|err| {
            tracing::warn!(
                owner = %owner.to_string(),
                error = %err,
                "embedding route refused; searching without a semantic arm"
            );
            EmbeddingRoute::none()
        })
    }

    /// Best-effort Fact embedding. Missing text or missing embedding
    /// client is a no-op; storage/LLM failures are returned to callers
    /// that explicitly requested embedding/backfill.
    ///
    /// # Errors
    ///
    /// Returns storage errors from text load/upsert, `Internal` for
    /// embedding client failures, and `ConstraintViolation` when the
    /// client returns a vector whose length differs from `dim()`.
    /// A Fact whose schema resolves to no embed unit is left alone:
    /// this is a no-op for it, not a forced vector.
    pub async fn ensure_fact_embedding(
        &self,
        owner: &OwnerId,
        memory_id: MemoryId,
    ) -> Result<(), StorageError> {
        self.ensure_memory_embedding(owner, EntityKind::Fact, memory_id)
            .await?;
        Ok(())
    }

    async fn ensure_memory_embedding(
        &self,
        owner: &OwnerId,
        entity_kind: EntityKind,
        memory_id: MemoryId,
    ) -> Result<bool, StorageError> {
        let route = self.embedding_route(owner).await?;
        let Some(client) = route.current_client() else {
            return Ok(false);
        };
        let step = self
            .embed_claimed_memory(client, owner, entity_kind, memory_id)
            .await?;
        Ok(matches!(step, EmbedStep::Embedded))
    }

    async fn embed_claimed_memory(
        &self,
        client: &BoundEmbeddingClient,
        owner: &OwnerId,
        entity_kind: EntityKind,
        memory_id: MemoryId,
    ) -> Result<EmbedStep, StorageError> {
        // This path holds a `MemoryId` and never passes through the job
        // queue, so the enqueue-side exclusions do not apply to it. The
        // schema's declaration is enforced here instead.
        let Some(text) = self
            .storage
            .ingest
            .embedding_text
            .load_embedding_texts_for_host(
                &[(*owner, entity_kind, memory_id)],
                self.registry().non_embeddable_schema_ids(),
                crate::storage_ports::OperatorMaintenanceProof::new(),
            )
            .await?
            .into_iter()
            .next()
            .flatten()
        else {
            return Ok(EmbedStep::NothingToEmbed);
        };
        let embedding = client
            .embed(&text)
            .await
            .map_err(|e| StorageError::Internal(format!("embed memory text: {e}")))?;
        self.storage
            .ingest
            .embedding_write
            .insert_embedding(
                owner,
                EmbeddableEntityRef::Memory {
                    kind: entity_kind,
                    memory_id,
                },
                &[super::memory_authoring::space_vector(client, embedding)?],
                crate::storage_ports::EmbeddingWriteProof::new(),
            )
            .await?;
        Ok(EmbedStep::Embedded)
    }

    /// Owner-scoped, idempotent backfill enqueue for memories missing an
    /// embedding in a space `owner`'s route names.
    ///
    /// Covers Facts *and* derived memories. Derived rows matter because a
    /// flavor can materialize Abstractions through its own sidecar path with
    /// no embedding client in scope (bulk ingest can), leaving them
    /// semantically invisible until someone ran a global reconcile. Only
    /// the kinds up to the caller's write limit are queued.
    ///
    /// # Errors
    ///
    /// Returns authorization failures with their protocol category
    /// (e.g. `Forbidden`) rather than an internal-error string, and
    /// storage errors from enqueueing missing jobs as `Internal`.
    pub async fn backfill_missing_embeddings(
        &self,
        authz: &AuthzContext,
        owner: &OwnerId,
        limit: usize,
    ) -> Result<usize, ProtocolError> {
        self.operation_authority(authz)?;
        let limit = i64::try_from(limit)
            .map_err(|_| ProtocolError::invalid_argument("limit", "too large"))?;
        let permit = self.authorize_write_limit(authz, owner).await?;
        let spaces = self.write_route(owner).await?.write_spaces();
        let mut enqueued = 0_u64;
        for space in &spaces {
            enqueued += self
                .storage
                .ingest
                .embedding_job
                .enqueue_missing_embedding_jobs(
                    permit.owner_write_permit(),
                    space,
                    limit,
                    self.registry().non_embeddable_schema_ids(),
                )
                .await
                .map_err(|err| ProtocolError::internal(err.to_string()))?;
        }
        usize::try_from(enqueued)
            .map_err(|_| ProtocolError::internal("enqueued count does not fit usize"))
    }

    /// Drain at most `limit` claimed jobs through each Owner's configured
    /// current and next clients. Stale spaces and missing text complete as
    /// no-ops. A heartbeat protects live claims; stale claims are reclaimed.
    ///
    /// Blocked clients release unsettled work and do not block other spaces.
    /// Only typed input refusals enter isolation and complete-coverage chunk
    /// rescue. Their final minimum-size refusal goes terminal; retryable and
    /// configuration failures retain pending work.
    ///
    /// # Errors
    ///
    /// Returns storage errors from claiming or final job-state writes. A
    /// storage error after a batch is claimed first returns that batch's
    /// unsettled claims to `pending` (best effort; the stale-claim timeout
    /// is the backstop), so a call that returns leaves none of its claims
    /// `processing`.
    /// Completed jobs and terminal input refusals count as processed;
    /// released jobs do not. Active observed configuration faults are
    /// returned even when no job completed.
    pub async fn drain_embedding_jobs(
        &self,
        limit: usize,
    ) -> Result<EmbeddingDrainOutcome, StorageError> {
        // Each pass pays the index debt head changes left since the last. A
        // host index that refuses or is down keeps its debt for the next
        // pass and holds no job back.
        let debt = i64::try_from(limit)
            .map_err(|_| StorageError::ConstraintViolation("limit too large".into()))?;
        if let Err(error) = self
            .storage
            .ingest
            .embedding_job
            .drain_vector_index_debt(debt)
            .await
        {
            tracing::warn!(error = %error, "vector index debt not paid this pass");
        }
        if self.embedding_router.is_none() {
            return Ok(EmbeddingDrainOutcome::default());
        }
        let policy = self.embedding_runtime_policy();
        self.reclaim_stale_embedding_claims(policy).await?;
        self.refresh_blocked_embedding_routes().await;
        let mut outcome = EmbeddingDrainOutcome::default();
        let mut remaining = limit;
        while remaining > 0 {
            let take = i64::try_from(remaining.min(policy.batch_size()))
                .map_err(|_| StorageError::ConstraintViolation("limit too large".into()))?;
            let now = tokio::time::Instant::now();
            let skip_owners = self.embedding_control.blocked_owners(now);
            let skip_owner_spaces = self.embedding_control.blocked_pairs(now);
            let claims = self
                .storage
                .ingest
                .embedding_job
                .claim_pending_embedding_jobs(take, &skip_owners, &skip_owner_spaces)
                .await?;
            if claims.is_empty() {
                break;
            }
            remaining = remaining.saturating_sub(claims.len());
            let _heartbeat = spawn_embedding_claim_heartbeat(
                self.storage.ingest.embedding_job.clone(),
                claims.clone(),
                policy.claim_heartbeat_interval(),
            );
            if let Err(err) = self
                .drain_claimed(claims.clone(), policy, &mut outcome)
                .await
            {
                self.release_unsettled(&claims, &err).await;
                return Err(err);
            }
        }
        outcome.configuration_faults = self.embedding_configuration_faults();
        Ok(outcome)
    }

    /// Settle one claimed batch: pair it with its texts, then embed it Owner
    /// by Owner. The one place a storage error leaves claims behind; the
    /// caller releases whatever this left `processing`.
    async fn drain_claimed(
        &self,
        claims: Vec<EmbeddingJobClaim>,
        policy: crate::EmbeddingRuntimePolicy,
        outcome: &mut EmbeddingDrainOutcome,
    ) -> Result<(), StorageError> {
        let batch = self.texted_claims(claims, outcome).await?;
        for (owner, owner_batch) in group_claims(batch, |(claim, _)| claim.owner) {
            self.drain_owner_batch(owner, owner_batch, policy, outcome)
                .await?;
        }
        Ok(())
    }

    /// After a storage error, return the batch's claims that are still
    /// `processing` to `pending`, so a caller that waits for the drain call
    /// to return (a quiesce) never outlives a claim.
    ///
    /// One fenced release per claim: a claim this batch already completed or
    /// failed answers `Conflict` and stays as it is. A release that fails for
    /// any other reason is logged and ends the sweep; `cause` is still what
    /// the caller returns, and the stale-claim timeout reclaims the rest.
    async fn release_unsettled(&self, claims: &[EmbeddingJobClaim], cause: &StorageError) {
        let reason = format!("embedding drain aborted: {cause}");
        for claim in claims {
            match self
                .storage
                .ingest
                .embedding_job
                .release_embedding_jobs(std::slice::from_ref(claim), &reason)
                .await
            {
                Ok(()) | Err(StorageError::Conflict(_)) => {}
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        %cause,
                        "could not release the claims of an aborted embedding drain; \
                         the stale-claim timeout reclaims them"
                    );
                    return;
                }
            }
        }
    }

    /// Pair each claim with its memory's embeddable text, completing the
    /// claims whose memory no longer yields any.
    ///
    /// Also excluded here, not just at enqueue: a job queued before its
    /// schema stopped resolving an embed unit completes as a no-op instead
    /// of embedding what now declines a vector.
    async fn texted_claims(
        &self,
        claims: Vec<EmbeddingJobClaim>,
        outcome: &mut EmbeddingDrainOutcome,
    ) -> Result<Vec<(EmbeddingJobClaim, String)>, StorageError> {
        let items: Vec<(OwnerId, EntityKind, MemoryId)> = claims
            .iter()
            .map(|claim| (claim.owner, claim.entity_kind, claim.entity_id))
            .collect();
        let texts = self
            .storage
            .ingest
            .embedding_text
            .load_embedding_texts_for_host(
                &items,
                self.registry().non_embeddable_schema_ids(),
                crate::storage_ports::OperatorMaintenanceProof::new(),
            )
            .await?;
        let mut batch = Vec::with_capacity(claims.len());
        for (claim, text) in claims.into_iter().zip(texts) {
            if let Some(text) = text {
                batch.push((claim, text));
            } else {
                self.complete_unembedded(&claim, outcome).await?;
            }
        }
        Ok(batch)
    }

    /// Embed one Owner's claims through that Owner's route, space by space.
    async fn drain_owner_batch(
        &self,
        owner: OwnerId,
        batch: Vec<(EmbeddingJobClaim, String)>,
        policy: crate::EmbeddingRuntimePolicy,
        outcome: &mut EmbeddingDrainOutcome,
    ) -> Result<(), StorageError> {
        let route = match self.embedding_route(&owner).await {
            Ok(route) => route,
            Err(err) => {
                let claims: Vec<EmbeddingJobClaim> =
                    batch.into_iter().map(|(claim, _)| claim).collect();
                return self
                    .back_off_owner(owner, policy, &claims, &format!("embedding route: {err}"))
                    .await;
            }
        };
        for (space, space_batch) in group_claims(batch, |(claim, _)| claim.space.clone()) {
            let Some(client) = route.client_for(&space) else {
                // The route moved on from this space since the job was
                // queued; embedding it would write a vector nothing reads.
                for (claim, _) in space_batch {
                    self.complete_unembedded(&claim, outcome).await?;
                }
                continue;
            };
            self.embed_claim_batch(client, space_batch, outcome).await?;
        }
        Ok(())
    }

    /// A route lookup failure backs off the Owner, independently of clients.
    async fn back_off_owner(
        &self,
        owner: OwnerId,
        policy: crate::EmbeddingRuntimePolicy,
        claims: &[EmbeddingJobClaim],
        reason: &str,
    ) -> Result<(), StorageError> {
        let retry_in = self.embedding_control.route_failed(
            owner,
            policy.worker_interval(),
            tokio::time::Instant::now(),
        );
        tracing::warn!(
            owner = %owner.to_string(),
            ?retry_in,
            reason,
            "embedding owner backed off; releasing its jobs"
        );
        if claims.is_empty() {
            return Ok(());
        }
        self.storage
            .ingest
            .embedding_job
            .release_embedding_jobs(claims, reason)
            .await
    }

    /// Complete a claim without a vector: its memory yields no text, or its
    /// Owner's route no longer names the claim's space.
    async fn complete_unembedded(
        &self,
        claim: &EmbeddingJobClaim,
        outcome: &mut EmbeddingDrainOutcome,
    ) -> Result<(), StorageError> {
        outcome.processed += 1;
        self.storage
            .ingest
            .embedding_job
            .complete_embedding_job(claim)
            .await
    }

    /// Only a typed input refusal isolates a batch. Other failures release
    /// unsettled claims; shared client control decides when they may retry.
    async fn embed_claim_batch(
        &self,
        client: &BoundEmbeddingClient,
        batch: Vec<(EmbeddingJobClaim, String)>,
        outcome: &mut EmbeddingDrainOutcome,
    ) -> Result<(), StorageError> {
        let texts: Vec<String> = batch.iter().map(|(_, text)| text.clone()).collect();
        match client.embed_many(&texts).await {
            Ok(vectors) => {
                let stored: Vec<_> = batch
                    .iter()
                    .zip(vectors)
                    .map(|((claim, _), vector)| (claim, vec![vector]))
                    .collect();
                let count = stored.len();
                self.store_claim_embeddings(client, stored).await?;
                outcome.processed += count;
                Ok(())
            }
            Err(LlmError::InputRefused(refusal)) => {
                let known_refusal = (batch.len() == 1).then_some(refusal);
                self.embed_claims_individually(client, batch, known_refusal, outcome)
                    .await
            }
            Err(err) => self.release_embedding_batch(batch, &err).await,
        }
    }

    async fn release_embedding_batch(
        &self,
        batch: Vec<(EmbeddingJobClaim, String)>,
        error: &LlmError,
    ) -> Result<(), StorageError> {
        let claims: Vec<EmbeddingJobClaim> = batch.into_iter().map(|(claim, _)| claim).collect();
        self.storage
            .ingest
            .embedding_job
            .release_embedding_jobs(&claims, &format!("embed memory text: {error}"))
            .await
    }

    /// Once per heartbeat interval, not per drain call: one UPDATE that frees
    /// what a dead drainer left `processing`. The heartbeat keeps a live
    /// drainer's claims inside the window, so this cannot steal in-flight
    /// work, and a worker that drains a backlog batch by batch pays for it
    /// once an interval.
    async fn reclaim_stale_embedding_claims(
        &self,
        policy: crate::EmbeddingRuntimePolicy,
    ) -> Result<(), StorageError> {
        if !self.embedding_control.reclaim_due(
            tokio::time::Instant::now(),
            policy.claim_heartbeat_interval(),
        ) {
            return Ok(());
        }
        let reclaimed = self
            .storage
            .ingest
            .embedding_job
            .reclaim_stale_embedding_jobs(policy.stale_claim_timeout_seconds())
            .await?;
        if reclaimed > 0 {
            tracing::info!(
                reclaimed,
                stale_after_seconds = policy.stale_claim_timeout_seconds(),
                "reclaimed abandoned processing embedding jobs before draining"
            );
        }
        Ok(())
    }

    /// Publish every claim's chunks and complete the claims in one
    /// transaction, each fenced on its token. A stale claim fails the batch
    /// before anything is written.
    async fn store_claim_embeddings(
        &self,
        client: &BoundEmbeddingClient,
        stored: Vec<(&EmbeddingJobClaim, Vec<Vec<f32>>)>,
    ) -> Result<(), StorageError> {
        let mut vectors = Vec::with_capacity(stored.len());
        for (_, chunks) in &stored {
            vectors.push(super::memory_authoring::space_vectors(
                client,
                chunks.clone(),
            )?);
        }
        let writes: Vec<EmbeddingWrite<'_>> = stored
            .iter()
            .zip(&vectors)
            .map(|((claim, _), vectors)| EmbeddingWrite {
                owner: &claim.owner,
                entity: EmbeddableEntityRef::Memory {
                    kind: claim.entity_kind,
                    memory_id: claim.entity_id,
                },
                vectors,
                proof: crate::storage_ports::EmbeddingWriteProof::for_claim(claim),
            })
            .collect();
        self.storage
            .ingest
            .embedding_write
            .insert_embeddings(&writes)
            .await?;
        Ok(())
    }

    /// Isolate an input-refused batch. A later non-input failure releases
    /// the current and remaining jobs, preserving complete coverage.
    async fn embed_claims_individually(
        &self,
        client: &BoundEmbeddingClient,
        batch: Vec<(EmbeddingJobClaim, String)>,
        mut known_refusal: Option<EmbeddingInputRefusal>,
        outcome: &mut EmbeddingDrainOutcome,
    ) -> Result<(), StorageError> {
        let mut pending = batch.into_iter();
        while let Some((claim, text)) = pending.next() {
            let attempt = match known_refusal.take() {
                Some(refusal) => Err(LlmError::InputRefused(refusal)),
                None => client.embed(&text).await,
            };
            let coverage = match attempt {
                Ok(vector) => Ok(ChunkedEmbedding::Embedded(vec![vector])),
                Err(LlmError::InputRefused(refusal)) => {
                    crate::llm::embed_in_chunks_after_failure(client.as_ref(), &text, refusal).await
                }
                Err(err) => Err(err),
            };
            match coverage {
                Ok(ChunkedEmbedding::Embedded(vectors)) => {
                    self.store_claim_embeddings(client, vec![(&claim, vectors)])
                        .await?;
                    outcome.processed += 1;
                }
                Ok(ChunkedEmbedding::InputRefused(refusal)) => {
                    self.storage
                        .ingest
                        .embedding_job
                        .fail_embedding_job_permanently(
                            &claim,
                            &format!("embed memory text: {refusal}"),
                        )
                        .await?;
                    outcome.processed += 1;
                    outcome.failed += 1;
                }
                Err(err) => {
                    let mut unsettled = vec![(claim, text)];
                    unsettled.extend(pending);
                    return self.release_embedding_batch(unsettled, &err).await;
                }
            }
        }
        Ok(())
    }

    /// Host-invoked reconciliation: enqueue durable embedding jobs for
    /// every embeddable memory in `scope` that lacks coverage in a space its
    /// Owner's route names. Complements [`Self::drain_embedding_jobs`]:
    /// drain heals the queue, reconcile heals the *absence* of queue entries
    /// (memories written while an Owner had no route, route changes,
    /// `failed` jobs whose retries are exhausted). Idempotent; like drain,
    /// the caller controls invocation — no worker or timer is spawned.
    ///
    /// Owners are read a page at a time and routed; Owners whose routes name
    /// the same space are reconciled in one pass. An Owner the host cannot
    /// route is skipped and logged: reconcile only heals, so there is no
    /// write to refuse.
    ///
    /// # Errors
    ///
    /// Returns storage errors from the owner scan and the
    /// reconciliation scan/enqueue. `limit: None` uses
    /// [`crate::EMBEDDING_RECONCILE_DEFAULT_LIMIT`]; it bounds the memories
    /// scanned across all Owners.
    pub async fn reconcile_embeddings(
        &self,
        scope: crate::EmbeddingReconcileScope,
        limit: Option<i64>,
    ) -> Result<crate::EmbeddingReconcileOutcome, StorageError> {
        const OWNER_PAGE: i64 = 1_000;
        let mut total = crate::EmbeddingReconcileOutcome::default();
        if self.embedding_router.is_none() {
            return Ok(total);
        }
        let maintenance = &self.storage.owner_inverse.embedding_maintenance;
        let mut budget = limit.unwrap_or(crate::EMBEDDING_RECONCILE_DEFAULT_LIMIT);
        let mut after: Option<OwnerId> = None;
        while budget > 0 {
            let page = maintenance
                .embedding_owner_page(
                    after,
                    OWNER_PAGE,
                    crate::storage_ports::OperatorMaintenanceProof::new(),
                )
                .await?;
            let Some(last) = page.last().copied() else {
                break;
            };
            let mut routed: Vec<(EmbeddingSpace, OwnerId)> = Vec::new();
            for owner in &page {
                match self.embedding_route(owner).await {
                    Ok(route) => routed.extend(
                        route
                            .write_spaces()
                            .into_iter()
                            .map(|space| (space, *owner)),
                    ),
                    Err(err) => tracing::warn!(
                        owner = %owner.to_string(),
                        error = %err,
                        "embedding route refused; not reconciling this owner"
                    ),
                }
            }
            for (space, owners) in group_claims(routed, |(space, _)| space.clone()) {
                if budget <= 0 {
                    break;
                }
                let owners: Vec<OwnerId> = owners.into_iter().map(|(_, owner)| owner).collect();
                let outcome = maintenance
                    .reconcile_embeddings(
                        crate::EmbeddingReconcileOptions {
                            space: &space,
                            owners: Some(&owners),
                            scope,
                            limit: Some(budget),
                            non_embeddable_schemas: self.registry().non_embeddable_schema_ids(),
                        },
                        self.embedding_runtime_policy(),
                        crate::storage_ports::OperatorMaintenanceProof::new(),
                    )
                    .await?;
                budget = budget.saturating_sub(i64::try_from(outcome.scanned).unwrap_or(i64::MAX));
                total.scanned += outcome.scanned;
                total.enqueued += outcome.enqueued;
                total.skipped += outcome.skipped;
            }
            if i64::try_from(page.len()).unwrap_or(i64::MAX) < OWNER_PAGE {
                break;
            }
            after = Some(last);
        }
        Ok(total)
    }

    /// Host-invoked: `owner`'s embedding state in every space its route
    /// names and every space it still has vectors or jobs in.
    ///
    /// Read it while an Owner moves to a new model: once the `Next` space's
    /// `embedded` reaches `embeddable` — less the inputs its provider
    /// refuses, counted in `failed_permanent` — the host can flip the route
    /// to `current(next)` and search moves with it. `Unrouted` rows are what
    /// [`Self::purge_embedding_spaces`] deletes.
    ///
    /// # Errors
    ///
    /// Storage errors from the counts; `Internal` when the host cannot
    /// route `owner`.
    pub async fn embedding_coverage(
        &self,
        owner: &OwnerId,
    ) -> Result<Vec<crate::EmbeddingSpaceCoverage>, StorageError> {
        let route = self.embedding_route(owner).await?;
        let rows = self
            .storage
            .owner_inverse
            .embedding_maintenance
            .embedding_coverage(
                owner,
                &route.write_spaces(),
                self.registry().non_embeddable_schema_ids(),
                crate::storage_ports::OperatorMaintenanceProof::new(),
            )
            .await?;
        let serves = |client: Option<&BoundEmbeddingClient>, space: &EmbeddingSpace| {
            client.is_some_and(|client| client.space() == space)
        };
        Ok(rows
            .into_iter()
            .map(|(space, counts)| {
                let role = if serves(route.current_client(), &space) {
                    crate::EmbeddingSpaceRole::Current
                } else if serves(route.next_client(), &space) {
                    crate::EmbeddingSpaceRole::Next
                } else {
                    crate::EmbeddingSpaceRole::Unrouted
                };
                crate::EmbeddingSpaceCoverage {
                    space,
                    role,
                    counts,
                }
            })
            .collect())
    }

    /// Host-invoked: delete `owner`'s vectors, heads and jobs in every space
    /// its route no longer names — the last step of a model move, and the
    /// offboarding step when an Owner's route becomes `none()`.
    ///
    /// Runs in batches until nothing is left. A `processing` job stays: its
    /// drain completes it as stale. A drain batch that resolved the route
    /// before the host flipped it can still land a vector in the old space
    /// after this returns; [`Self::embedding_coverage`] shows it and a second
    /// purge removes it.
    ///
    /// # Errors
    ///
    /// `Internal` when no embedding router is installed — the engine would
    /// read that as "no spaces" and delete everything — or when the host
    /// cannot route `owner`; neither deletes anything. Storage errors from
    /// the purge, which leave earlier batches committed.
    pub async fn purge_embedding_spaces(
        &self,
        owner: &OwnerId,
    ) -> Result<crate::EmbeddingPurgeOutcome, StorageError> {
        const BATCH: i64 = 1_000;
        if self.embedding_router.is_none() {
            return Err(StorageError::Internal(
                "no embedding router is installed; refusing to purge every space".into(),
            ));
        }
        let keep = self.embedding_route(owner).await?.write_spaces();
        let maintenance = &self.storage.owner_inverse.embedding_maintenance;
        let mut total = crate::EmbeddingPurgeOutcome::default();
        loop {
            let batch = maintenance
                .purge_embedding_spaces(
                    owner,
                    &keep,
                    BATCH,
                    crate::storage_ports::OperatorMaintenanceProof::new(),
                )
                .await?;
            total += batch;
            let cut = u64::try_from(BATCH).unwrap_or(u64::MAX);
            if batch.vectors < cut && batch.heads < cut && batch.jobs < cut {
                return Ok(total);
            }
        }
    }
}
