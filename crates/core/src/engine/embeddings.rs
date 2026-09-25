//! How the engine embeds: each Owner's route, the inline embed, the durable
//! job drain, and the host's backfill, reconcile, coverage and purge.

use std::sync::Arc;

use super::Engine;
use crate::access::Relation;
use crate::authz::AuthzContext;
use crate::error::ProtocolError;
use crate::llm::{
    BoundEmbeddingClient, EmbeddingClient, EmbeddingRoute, EmbeddingRouteError, EmbeddingSpace,
    LlmError,
};
use crate::storage::{EmbeddingJobClaim, StorageError};
use crate::storage_ports::EmbeddingJobHandle;
use crate::{EmbeddableEntityRef, EntityKind, MemoryId, Owner};

/// Liveness probe after a provider refuses a batch.
///
/// Trivial and constant: a failed probe means the provider is down; a
/// successful one means the refused batch's contents are at fault. Shared
/// with [`crate::llm::embed_failure_blames_the_input`] so drain and write
/// ask the same question.
const TRANSIENT_BATCH_PROBE: &str = crate::llm::EMBED_LIVENESS_PROBE;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddingDrainOutcome {
    pub processed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmbedStep {
    Embedded,
    NothingToEmbed,
}

/// Longest a failing Owner's jobs wait between drain attempts.
const MAX_EMBEDDING_BACKOFF: std::time::Duration = std::time::Duration::from_mins(15);

/// Owners whose route or provider failed, and until when the drain skips
/// their jobs.
///
/// One Owner's broken endpoint must not stall the queue for every other
/// Owner, and must not be hammered either: its claims are released and it
/// waits `worker_interval`, doubling per consecutive failure up to
/// [`MAX_EMBEDDING_BACKOFF`]. A successful batch clears it. In memory only:
/// a restart retries every Owner once.
#[derive(Debug, Default)]
pub(crate) struct EmbeddingBackoff {
    owners: std::sync::Mutex<std::collections::HashMap<Owner, OwnerBackoff>>,
}

#[derive(Debug, Clone, Copy)]
struct OwnerBackoff {
    until: tokio::time::Instant,
    delay: std::time::Duration,
}

impl EmbeddingBackoff {
    fn owners(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<Owner, OwnerBackoff>> {
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Owners whose delay has not passed.
    fn blocked(&self) -> Vec<Owner> {
        let now = tokio::time::Instant::now();
        self.owners()
            .iter()
            .filter(|(_, backoff)| backoff.until > now)
            .map(|(owner, _)| *owner)
            .collect()
    }

    /// Record a failure; returns how long the Owner now waits.
    fn fail(&self, owner: Owner, base: std::time::Duration) -> std::time::Duration {
        let mut owners = self.owners();
        let cap = MAX_EMBEDDING_BACKOFF.max(base);
        let delay = owners
            .get(&owner)
            .map_or(base, |prior| prior.delay.saturating_mul(2))
            .min(cap);
        owners.insert(
            owner,
            OwnerBackoff {
                until: tokio::time::Instant::now() + delay,
                delay,
            },
        );
        delay
    }

    fn succeed(&self, owner: &Owner) {
        self.owners().remove(owner);
    }
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

#[derive(Debug)]
struct RequestTimeoutEmbeddingClient {
    inner: Arc<dyn EmbeddingClient>,
    request_timeout: std::time::Duration,
}

#[async_trait::async_trait]
impl EmbeddingClient for RequestTimeoutEmbeddingClient {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, crate::llm::LlmError> {
        crate::llm::embed_with_timeout(self.inner.as_ref(), text, self.request_timeout).await
    }

    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, crate::llm::LlmError> {
        crate::llm::embed_many_with_timeout(self.inner.as_ref(), texts, self.request_timeout).await
    }

    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn dim(&self) -> usize {
        self.inner.dim()
    }
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
        owner: &Owner,
    ) -> Result<EmbeddingRoute, EmbeddingRouteError> {
        let Some(router) = &self.embedding_router else {
            return Ok(EmbeddingRoute::none());
        };
        let request_timeout = self.embedding_runtime_policy.request_timeout();
        Ok(router.route(owner).await?.wrap_clients(|inner| {
            Arc::new(RequestTimeoutEmbeddingClient {
                inner,
                request_timeout,
            })
        }))
    }

    /// `owner`'s route for a write: a route the host refuses refuses the
    /// write. The host's message can name its own configuration, so it
    /// stays in the log.
    pub(in crate::engine) async fn write_route(
        &self,
        owner: &Owner,
    ) -> Result<EmbeddingRoute, ProtocolError> {
        self.embedding_route(owner).await.map_err(|err| {
            tracing::warn!(
                owner = %owner.external_key(),
                error = %err,
                "embedding route refused"
            );
            ProtocolError::internal("no embedding route for this owner")
        })
    }

    /// `owner`'s route for a search: a route the host refuses reads as
    /// [`EmbeddingRoute::none`], so the search runs without a semantic arm
    /// instead of failing.
    pub async fn search_route(&self, owner: &Owner) -> EmbeddingRoute {
        self.embedding_route(owner).await.unwrap_or_else(|err| {
            tracing::warn!(
                owner = %owner.external_key(),
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
        owner: &Owner,
        memory_id: MemoryId,
    ) -> Result<(), StorageError> {
        self.ensure_memory_embedding(owner, EntityKind::Fact, memory_id)
            .await?;
        Ok(())
    }

    async fn ensure_memory_embedding(
        &self,
        owner: &Owner,
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
        owner: &Owner,
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
                &super::memory_authoring::space_vector(client, embedding)?,
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
    /// no embedding client in scope — code-chunk ingest does — leaving them
    /// semantically invisible until someone ran a global reconcile.
    ///
    /// # Errors
    ///
    /// Returns authorization failures with their protocol category
    /// (e.g. `Forbidden`) rather than an internal-error string, and
    /// storage errors from enqueueing missing jobs as `Internal`.
    pub async fn backfill_missing_embeddings(
        &self,
        authz: &AuthzContext,
        owner: &Owner,
        limit: usize,
    ) -> Result<usize, ProtocolError> {
        self.operation_authority(authz)?;
        let limit = i64::try_from(limit)
            .map_err(|_| ProtocolError::invalid_argument("limit", "too large"))?;
        let permit = self.authorize_write(authz, owner, Relation::Ingest).await?;
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

    /// Host-invoked sweep that drains durable pending memory embedding
    /// jobs, each through the route of the Owner whose memory it embeds.
    /// This method does not spawn a worker, timer, or model decision loop;
    /// the caller controls invocation and `limit`. Jobs are claimed in
    /// batches of up to the host-configured
    /// [`crate::EmbeddingRuntimePolicy::batch_size`] and sent to each
    /// Owner's client grouped by Owner and space. Direct core hosts can
    /// install the policy with [`Engine::with_embedding_runtime_policy`].
    ///
    /// Every invocation first returns `processing` claims older than the
    /// policy's stale-claim timeout to `pending` (one statement, all
    /// spaces), then claims. A drainer that died holding a claim — a process
    /// stopped between claim and completion — is recovered here, not only at
    /// boot.
    ///
    /// Routing semantics:
    /// - a route error releases that Owner's claims and backs the Owner off
    ///   (from the policy's worker interval, doubling to 15 minutes, reset
    ///   by a successful batch); other Owners keep draining;
    /// - a job for a space the Owner's route no longer names is stale and
    ///   completes without a vector — reconcile queues the route's space.
    ///
    /// Failure semantics:
    /// - a *transient* batch failure (429/5xx/network) releases the Owner's
    ///   claimed jobs back to `pending` without burning retry attempts — a
    ///   provider outage is not evidence against any individual job — and
    ///   backs that Owner off;
    /// - a batch failure whose liveness probe succeeds re-embeds the batch one
    ///   text at a time to isolate the content-attributed input(s); an
    ///   over-limit input is rescued by bisecting it into chunks and storing
    ///   the first chunk's vector, jobs rejected at every length go terminal instead of
    ///   cycling reject-retry forever, and their batch-mates still embed;
    ///   when the probe fails, all jobs remain retryable.
    ///
    /// # Errors
    ///
    /// Returns storage errors from claiming or final job-state writes.
    /// Per-job embedding failures are recorded on their job rows and
    /// counted in the returned outcome; each job receives at most one
    /// attempt per invocation.
    pub async fn drain_embedding_jobs(
        &self,
        limit: usize,
    ) -> Result<EmbeddingDrainOutcome, StorageError> {
        if self.embedding_router.is_none() {
            return Ok(EmbeddingDrainOutcome::default());
        }
        let policy = self.embedding_runtime_policy();
        self.reclaim_stale_embedding_claims(policy).await?;
        let mut outcome = EmbeddingDrainOutcome::default();
        let mut remaining = limit;
        while remaining > 0 {
            let take = i64::try_from(remaining.min(policy.batch_size()))
                .map_err(|_| StorageError::ConstraintViolation("limit too large".into()))?;
            let claims = self
                .storage
                .ingest
                .embedding_job
                .claim_pending_embedding_jobs(take, &self.embedding_backoff.blocked())
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
            let batch = self.texted_claims(claims, &mut outcome).await?;
            for (owner, owner_batch) in group_claims(batch, |(claim, _)| claim.owner) {
                self.drain_owner_batch(owner, owner_batch, policy, &mut outcome)
                    .await?;
            }
        }
        Ok(outcome)
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
        let items: Vec<(Owner, EntityKind, MemoryId)> = claims
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
        owner: Owner,
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
        let mut spaces = group_claims(batch, |(claim, _)| claim.space.clone()).into_iter();
        while let Some((space, space_batch)) = spaces.next() {
            let Some(client) = route.client_for(&space) else {
                // The route moved on from this space since the job was
                // queued; embedding it would write a vector nothing reads.
                for (claim, _) in space_batch {
                    self.complete_unembedded(&claim, outcome).await?;
                }
                continue;
            };
            if !self.embed_claim_batch(client, space_batch, outcome).await? {
                let rest: Vec<EmbeddingJobClaim> = spaces
                    .flat_map(|(_, rest)| rest.into_iter().map(|(claim, _)| claim))
                    .collect();
                return self
                    .back_off_owner(owner, policy, &rest, "embedding provider unavailable")
                    .await;
            }
        }
        self.embedding_backoff.succeed(&owner);
        Ok(())
    }

    /// Back `owner` off and release `claims` without burning an attempt:
    /// its route or provider failed, and none of them was tried.
    async fn back_off_owner(
        &self,
        owner: Owner,
        policy: crate::EmbeddingRuntimePolicy,
        claims: &[EmbeddingJobClaim],
        reason: &str,
    ) -> Result<(), StorageError> {
        let retry_in = self.embedding_backoff.fail(owner, policy.worker_interval());
        tracing::warn!(
            owner = %owner.external_key(),
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

    /// One provider batch for one Owner and space. `false` when the
    /// provider is down: the batch's claims are released for a later drain.
    async fn embed_claim_batch(
        &self,
        client: &BoundEmbeddingClient,
        batch: Vec<(EmbeddingJobClaim, String)>,
        outcome: &mut EmbeddingDrainOutcome,
    ) -> Result<bool, StorageError> {
        let texts: Vec<String> = batch.iter().map(|(_, text)| text.clone()).collect();
        match client.embed_many(&texts).await {
            Ok(vectors) if vectors.len() != batch.len() => {
                self.release_malformed_embedding_batch(batch, vectors.len())
                    .await?;
                Ok(false)
            }
            Ok(vectors) => {
                for ((claim, _), vector) in batch.iter().zip(vectors) {
                    outcome.processed += 1;
                    if !self.store_claim_embedding(client, claim, vector).await? {
                        outcome.failed += 1;
                    }
                }
                Ok(true)
            }
            Err(LlmError::EmbedPermanent(_)) => {
                self.embed_claims_individually(client, batch, outcome)
                    .await?;
                Ok(true)
            }
            Err(err) => {
                self.recover_transient_embedding_batch(client, batch, outcome, &err)
                    .await
            }
        }
    }

    /// A transient batch error is supposed to mean the provider failed
    /// rather than any input being bad — but the two are indistinguishable
    /// from the response when the provider fails *because of* an input.
    /// Observed against a local runner: one scanned page whose OCR
    /// hallucinated a 300-row CJK table killed the model process, which
    /// surfaces as `400 {"error": "… EOF"}`, correctly classified transient
    /// because nothing looked at the input. Released unburned, the whole
    /// claim of 32 came back every drain and 31 innocent pages of the book
    /// stayed unembedded indefinitely.
    ///
    /// Probing separates the cases. If the provider answers a trivial input
    /// right after refusing the batch, it is up, and this batch's failure is
    /// attributable to its contents — so isolate them the same way a
    /// permanent rejection is isolated, and the drain continues (`true`).
    /// If the probe also fails, the provider really is down: release
    /// without burning attempts, exactly as before, for one extra tiny
    /// call, and the Owner backs off (`false`).
    async fn recover_transient_embedding_batch(
        &self,
        client: &BoundEmbeddingClient,
        batch: Vec<(EmbeddingJobClaim, String)>,
        outcome: &mut EmbeddingDrainOutcome,
        err: &LlmError,
    ) -> Result<bool, StorageError> {
        if client.embed(TRANSIENT_BATCH_PROBE).await.is_ok() {
            tracing::warn!(
                error = %err,
                jobs = batch.len(),
                "transient embedding batch failure but the provider answers; \
                 isolating inputs instead of holding the batch"
            );
            self.embed_claims_individually(client, batch, outcome)
                .await?;
            return Ok(true);
        }
        let claims: Vec<EmbeddingJobClaim> = batch.into_iter().map(|(claim, _)| claim).collect();
        tracing::warn!(
            error = %err,
            jobs = claims.len(),
            "transient embedding batch failure; releasing claims without burning attempts"
        );
        self.storage
            .ingest
            .embedding_job
            .release_embedding_jobs(&claims, &format!("embed memory text: {err}"))
            .await?;
        Ok(false)
    }

    /// Once per drain, not per batch: one UPDATE that frees what a dead
    /// drainer left `processing`. The heartbeat keeps a live drainer's
    /// claims inside the window, so this cannot steal in-flight work.
    async fn reclaim_stale_embedding_claims(
        &self,
        policy: crate::EmbeddingRuntimePolicy,
    ) -> Result<(), StorageError> {
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

    async fn release_malformed_embedding_batch(
        &self,
        batch: Vec<(EmbeddingJobClaim, String)>,
        received: usize,
    ) -> Result<(), StorageError> {
        let error = format!(
            "embedding batch cardinality mismatch: sent {} texts but received {received} vectors",
            batch.len(),
        );
        tracing::warn!(
            sent = batch.len(),
            received,
            "embedding provider returned malformed batch cardinality"
        );
        let claims: Vec<EmbeddingJobClaim> = batch.into_iter().map(|(claim, _)| claim).collect();
        self.storage
            .ingest
            .embedding_job
            .release_embedding_jobs(&claims, &error)
            .await
    }

    /// Store one produced vector for its claim and complete the job; a
    /// vector of another width than the client declared records an
    /// ordinary retryable job failure instead. Returns whether the vector
    /// was stored.
    async fn store_claim_embedding(
        &self,
        client: &BoundEmbeddingClient,
        claim: &EmbeddingJobClaim,
        vector: Vec<f32>,
    ) -> Result<bool, StorageError> {
        let vector = match client.vector(vector) {
            Ok(vector) => vector,
            Err(err) => {
                self.storage
                    .ingest
                    .embedding_job
                    .fail_embedding_job(claim, &err.to_string())
                    .await?;
                return Ok(false);
            }
        };
        self.storage
            .ingest
            .embedding_write
            .insert_embedding(
                &claim.owner,
                EmbeddableEntityRef::Memory {
                    kind: claim.entity_kind,
                    memory_id: claim.entity_id,
                },
                &vector,
                crate::storage_ports::EmbeddingWriteProof::for_claim(claim),
            )
            .await?;
        self.storage
            .ingest
            .embedding_job
            .complete_embedding_job(claim)
            .await?;
        Ok(true)
    }

    /// Per-item fallback after a live-provider batch rejection: isolate which
    /// inputs the provider rejects. A rejected input is bisected into
    /// provider-acceptable chunks ([`crate::llm::embed_in_chunks_after_failure`])
    /// and its first chunk is stored — storage keeps one vec per version —
    /// so an over-limit input stays findable instead of going invisible.
    /// An ambiguous per-item failure
    /// is eligible only after its own liveness probe succeeds.
    /// Inputs the provider rejects at every length go terminal; other
    /// errors record one ordinary attempt. Either way each job gets at
    /// most one attempt in this pass.
    async fn embed_claims_individually(
        &self,
        client: &BoundEmbeddingClient,
        batch: Vec<(EmbeddingJobClaim, String)>,
        outcome: &mut EmbeddingDrainOutcome,
    ) -> Result<(), StorageError> {
        for (claim, text) in batch {
            outcome.processed += 1;
            match client.embed(&text).await {
                Ok(vector) => {
                    if !self.store_claim_embedding(client, &claim, vector).await? {
                        outcome.failed += 1;
                    }
                }
                Err(err) => {
                    let initial_error = err.to_string();
                    match crate::llm::embed_in_chunks_after_failure(client.as_ref(), &text, err)
                        .await
                    {
                        Ok(Some(vectors)) => {
                            tracing::warn!(
                                entity_id = ?claim.entity_id,
                                chunks = vectors.len(),
                                total_bytes = text.len(),
                                "over-limit embedding input rescued by its first chunk"
                            );
                            let stored = if let Some(first) = vectors.into_iter().next() {
                                self.store_claim_embedding(client, &claim, first).await?
                            } else {
                                self.storage
                                    .ingest
                                    .embedding_job
                                    .fail_embedding_job(
                                        &claim,
                                        "chunked embedding rescue returned no chunks",
                                    )
                                    .await?;
                                false
                            };
                            if !stored {
                                outcome.failed += 1;
                            }
                        }
                        Ok(None) => {
                            outcome.failed += 1;
                            tracing::warn!(
                                entity_id = ?claim.entity_id,
                                "embedding input permanently rejected at every length; job going terminal"
                            );
                            self.storage
                                .ingest
                                .embedding_job
                                .fail_embedding_job_permanently(
                                    &claim,
                                    &format!("embed memory text: {initial_error}"),
                                )
                                .await?;
                        }
                        Err(err) => {
                            outcome.failed += 1;
                            self.storage
                                .ingest
                                .embedding_job
                                .fail_embedding_job(
                                    &claim,
                                    &format!("embed truncated memory text: {err}"),
                                )
                                .await?;
                        }
                    }
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
        let mut after: Option<Owner> = None;
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
            let mut routed: Vec<(EmbeddingSpace, Owner)> = Vec::new();
            for owner in &page {
                match self.embedding_route(owner).await {
                    Ok(route) => routed.extend(
                        route
                            .write_spaces()
                            .into_iter()
                            .map(|space| (space, *owner)),
                    ),
                    Err(err) => tracing::warn!(
                        owner = %owner.external_key(),
                        error = %err,
                        "embedding route refused; not reconciling this owner"
                    ),
                }
            }
            for (space, owners) in group_claims(routed, |(space, _)| space.clone()) {
                if budget <= 0 {
                    break;
                }
                let owners: Vec<Owner> = owners.into_iter().map(|(_, owner)| owner).collect();
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
        owner: &Owner,
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
        owner: &Owner,
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
