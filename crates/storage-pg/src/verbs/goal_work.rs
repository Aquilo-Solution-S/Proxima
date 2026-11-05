use proxiima_core::storage::{AuthorDerivedOutcome, AuthorDerivedRequest, StorageError};
use proxiima_core::storage_ports::OperatorWriteProof;
use proxiima_core::verbs::idempotency::{ContentFingerprint, KeyScope, RequestKey};
use proxiima_core::{EdgeEndpoint, EntityKind, GoalWorkPermit, MemoryTarget};
use sqlx::Row;
use std::collections::BTreeSet;

use crate::{PgStorage, verbs};

struct GoalWorkAuthority {
    goal: proxiima_core::GoalHandle,
    goal_owner: proxiima_core::OwnerId,
    generation: i64,
    endpoints: Vec<EdgeEndpoint>,
    new_origins: Vec<EdgeEndpoint>,
    new_references: Vec<EdgeEndpoint>,
    source_reads: crate::ports::goal_collaboration::source_reads::SourceReads,
    roles: proxiima_core::OwnerRoles,
}

async fn prepare_goal_work_authority(
    storage: &PgStorage,
    req: &AuthorDerivedRequest<'_>,
    permit: &GoalWorkPermit,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<GoalWorkAuthority, StorageError> {
    let goal = permit.command().goal();
    let owner_hint: uuid::Uuid =
        sqlx::query_scalar("SELECT owner_id FROM proxiima_core.goal_head WHERE handle = $1")
            .bind(goal.into_inner())
            .fetch_optional(&mut **tx)
            .await
            .map_err(crate::error::map_err)?
            .ok_or_else(|| {
                StorageError::ConstraintViolation("Goal work Goal head not found".into())
            })?;
    let hinted_owner = proxiima_core::OwnerId::new(owner_hint);
    let new_origins = verbs::derive_append::new_origins(req.origins, req.retained_pins);
    let new_references = verbs::derive_append::new_references(req.references, req.retained_pins);
    let mut endpoints: Vec<EdgeEndpoint> =
        new_origins.iter().chain(&new_references).copied().collect();
    if let Some(retained) = req.retained_pins {
        endpoints.push(EdgeEndpoint::memory(retained.kind(), retained.prior()));
    }
    endpoints.extend(
        req.guided_by
            .iter()
            .copied()
            .map(|id| EdgeEndpoint::goal(proxiima_core::GoalId::new(id.into_inner()))),
    );
    let source_reads = crate::ports::goal_collaboration::source_reads::SourceReads::discover(
        storage,
        tx,
        permit.command().scope(),
        &endpoints,
    )
    .await?;
    let mut owner_ids: BTreeSet<uuid::Uuid> = source_reads
        .owners()
        .map(proxiima_core::OwnerId::into_inner)
        .collect();
    owner_ids.insert(hinted_owner.into_inner());
    let owner_ids: Vec<_> = owner_ids
        .into_iter()
        .map(proxiima_core::OwnerId::new)
        .collect();
    let roles = crate::ports::goal_collaboration::fenced_roles(
        storage,
        tx,
        permit.command().scope(),
        &owner_ids,
        &[],
    )
    .await?;
    source_reads.lock_goals(tx, (goal, hinted_owner)).await?;
    let goal_row = sqlx::query(
        "SELECT gh.owner_id, gh.authority_generation, g.state::text AS state
           FROM proxiima_core.goal_head gh
           JOIN proxiima_core.goal g ON g.handle = gh.handle AND g.t = gh.t
          WHERE gh.handle = $1
          FOR UPDATE",
    )
    .bind(goal.into_inner())
    .fetch_optional(&mut **tx)
    .await
    .map_err(crate::error::map_err)?
    .ok_or_else(|| StorageError::ConstraintViolation("Goal work Goal head not found".into()))?;
    let goal_owner = proxiima_core::OwnerId::new(goal_row.get("owner_id"));
    if goal_owner != permit.owner() || goal_row.get::<String, _>("state") != "Active" {
        return Err(StorageError::ConstraintViolation(
            "Goal work Goal is not active or owner mismatched".into(),
        ));
    }
    if goal_owner != hinted_owner {
        return Err(StorageError::Retryable(
            "Goal owner changed while acquiring authority fences".into(),
        ));
    }
    let current = crate::ports::goal_collaboration::current_goal_role(
        tx,
        permit.command(),
        &roles,
        goal_row.get::<i64, _>("authority_generation"),
    )
    .await?;
    let write_rank = current.map_or(0, |role| i16::from(role.write_ceiling().rank()));
    if write_rank < i16::from(permit.kind().rank()) {
        return Err(StorageError::ConstraintViolation(format!(
            "Goal work write ceiling denied: rank={write_rank} required={}",
            permit.kind().rank()
        )));
    }
    Ok(GoalWorkAuthority {
        goal,
        goal_owner,
        generation: goal_row.get("authority_generation"),
        endpoints,
        new_origins,
        new_references,
        source_reads,
        roles,
    })
}

async fn validate_goal_work_target(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    req: &AuthorDerivedRequest<'_>,
    permit: &GoalWorkPermit,
    draft: &mut verbs::derive_append::DerivedDraft<'_>,
    goal: proxiima_core::GoalHandle,
    goal_owner: proxiima_core::OwnerId,
    goal_generation: i64,
) -> Result<(), StorageError> {
    if let Some(request) = draft.request.take() {
        let scope = match req.kind {
            EntityKind::Abstraction => KeyScope::GoalAbstraction(goal, permit.command().subject()),
            EntityKind::Perspective => KeyScope::GoalPerspective(goal, permit.command().subject()),
            _ => {
                return Err(StorageError::ConstraintViolation(
                    "Goal work only supports Abstraction or Perspective idempotency scopes".into(),
                ));
            }
        };
        let request = if request.scope() == scope {
            RequestKey::new(scope, request.key().clone(), request.fingerprint())
        } else {
            let fingerprint = ContentFingerprint::builder(scope)
                .bytes("original-fingerprint", request.fingerprint().as_bytes())
                .build();
            RequestKey::new(scope, request.key().clone(), fingerprint)
        };
        draft.request = Some(request);
    }
    match permit.target() {
        MemoryTarget::Series(series) => {
            if req.supersedes.is_some() || req.memory_id.into_inner() != series.into_inner() {
                return Err(StorageError::ConstraintViolation(
                    "Goal work series mismatch".into(),
                ));
            }
            let existing: Option<(uuid::Uuid, i64)> = sqlx::query_as(
                "SELECT handle, authority_generation FROM proxiima_core.memory_head WHERE handle=$1 AND owner_id=$2",
            )
            .bind(series.into_inner())
            .bind(goal_owner.into_inner())
            .fetch_optional(&mut **tx)
            .await
            .map_err(crate::error::map_err)?;
            if let Some((_, generation)) = existing {
                let bound: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM proxiima_core.goal_work_binding
                       WHERE goal_handle=$1 AND memory_handle=$2 AND source_owner=$3
                         AND source_generation=$4 AND controller_generation=$5)",
                )
                .bind(goal.into_inner())
                .bind(series.into_inner())
                .bind(goal_owner.into_inner())
                .bind(generation)
                .bind(goal_generation)
                .fetch_one(&mut **tx)
                .await
                .map_err(crate::error::map_err)?;
                if !bound {
                    return Err(StorageError::NotFound);
                }
            }
        }
        MemoryTarget::Revision(prior) => {
            if req.supersedes != Some(prior) {
                return Err(StorageError::ConstraintViolation(
                    "Goal work revision mismatch".into(),
                ));
            }
            let bound: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                   SELECT 1 FROM proxiima_core.goal_work_binding w
                   JOIN proxiima_core.memory_head h ON h.handle = w.memory_handle
                   JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (work revision under owner RLS) */ ON m.handle = h.handle AND m.forgotten_at IS NULL
                  WHERE w.goal_handle = $1 AND m.t = $2
                    AND w.source_owner = $3 AND w.controller_generation = $4
                    AND w.source_generation = h.authority_generation AND h.owner_id = w.source_owner
                )",
            )
            .bind(goal.into_inner())
            .bind(prior.into_inner())
            .bind(goal_owner.into_inner())
            .bind(goal_generation)
            .fetch_one(&mut **tx)
            .await
            .map_err(crate::error::map_err)?;
            if !bound {
                return Err(StorageError::NotFound);
            }
        }
    }
    Ok(())
}

async fn admit_goal_work(
    storage: &PgStorage,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    req: &AuthorDerivedRequest<'_>,
    permit: &GoalWorkPermit,
    draft: &verbs::derive_append::DerivedDraft<'_>,
    goal: proxiima_core::GoalHandle,
    goal_owner: proxiima_core::OwnerId,
) -> Result<(verbs::derive_append::DerivedOutcome, usize), StorageError> {
    let sidecars = storage.sidecars.writing_derived(draft);
    let sidecar_payload = req.sidecar_payload.clone();
    let content_payload = sidecar_payload.clone();
    let tables = storage
        .sidecars
        .tables_for_payloads(std::slice::from_ref(&sidecar_payload))?;
    let scopes = storage
        .scopes
        .targets_for_payloads(std::slice::from_ref(&sidecar_payload))?;
    let outcome = verbs::derive_append::append_goal_work_with_content_payloads_in_tx(
        tx,
        permit,
        draft,
        verbs::derive_append::DerivedAdmissionInput {
            origins: req.origins,
            references: req.references,
            sidecar_tables: &tables,
            scopes: &scopes,
            content: verbs::derive_append::ContentResolution {
                content_id: None,
                payloads: Some(std::slice::from_ref(&content_payload)),
            },
        },
        move |tx, outcome| {
            Box::pin(async move {
                sidecars
                    .insert_memory_sidecar(tx, outcome.memory_id, &sidecar_payload)
                    .await
            })
        },
    )
    .await?;
    if matches!(permit.target(), MemoryTarget::Series(_)) && !outcome.idempotent_replay {
        sqlx::query(
            "INSERT INTO proxiima_core.goal_work_binding
                (goal_handle, memory_handle, source_owner, source_generation,
                 controller_generation, bound_by)
             SELECT $1, h.handle, h.owner_id, h.authority_generation,
                    gh.authority_generation, $2
               FROM proxiima_core.memory_head h
               JOIN proxiima_core.goal_head gh ON gh.handle = $1
              WHERE h.handle = $3 AND h.owner_id = $4
             ON CONFLICT (goal_handle, memory_handle) DO NOTHING",
        )
        .bind(goal.into_inner())
        .bind(permit.command().subject().into_inner())
        .bind(
            sqlx::query_scalar::<_, uuid::Uuid>(
                "SELECT handle FROM proxiima_core.memory /* FORGET-POLICY: live-only (new work admission under owner RLS) */ WHERE t = $1 AND forgotten_at IS NULL",
            )
            .bind(outcome.memory_id.into_inner())
            .fetch_one(&mut **tx)
            .await
            .map_err(crate::error::map_err)?,
        )
        .bind(goal_owner.into_inner())
        .execute(&mut **tx)
        .await
        .map_err(crate::error::map_err)?;
    }
    if !outcome.idempotent_replay {
        verbs::sketch::upsert_sketch(
            tx,
            permit.owner().into_inner(),
            outcome.memory_id.into_inner(),
            req.kind.into(),
            &verbs::sketch::sketch_line(
                req.kind.into(),
                Some(req.text.as_str()),
                std::slice::from_ref(&req.sidecar_payload),
            ),
        )
        .await?;
    }
    let edge_count = verbs::derive_append::assert_derived_index_rows(
        tx,
        draft,
        &outcome,
        req.origins,
        req.references,
    )
    .await?
    .saturating_add(
        verbs::derive_append::record_external_edges(
            tx,
            &outcome,
            req.external_origins,
            req.external_references,
        )
        .await?,
    );
    Ok((outcome, edge_count))
}

/// Admit one A/P version through an already-authorized Goal work permit.
/// The permit is a distinct capability: no owner-write permit is created.
pub(crate) async fn author_work_memory(
    storage: &PgStorage,
    req: &AuthorDerivedRequest<'_>,
    permit: &GoalWorkPermit,
    _proof: OperatorWriteProof,
) -> Result<AuthorDerivedOutcome, StorageError> {
    if req.owner != permit.owner() || proxiima_core::AccessKind::from(req.kind) != permit.kind() {
        return Err(StorageError::ConstraintViolation(
            "Goal work draft does not match its sealed destination".into(),
        ));
    }
    let mut draft = verbs::derive_append::DerivedDraft::from_request(req, storage.store_number);
    let mut tx = storage.platform_transaction().await?;
    sqlx::query("SELECT set_config('app.goal_subject', $1::uuid::text, true)")
        .bind(permit.command().subject().into_inner())
        .execute(&mut *tx)
        .await
        .map_err(crate::error::map_err)?;
    let authority = prepare_goal_work_authority(storage, req, permit, &mut tx).await?;
    let goal = authority.goal;
    let goal_owner = authority.goal_owner;
    let goal_generation = authority.generation;
    let endpoints = authority.endpoints;
    let new_origins = authority.new_origins;
    let new_references = authority.new_references;
    let source_reads = authority.source_reads;
    let roles = authority.roles;
    verbs::derive_append::validate_retained_pins_in_tx(&mut tx, &draft, req.retained_pins).await?;
    // Validate source pins under the caller's original RLS scope on this same
    // transaction, then restore the platform scope before materialization.
    crate::owner_scope::bind_owner_scope(&mut tx, permit.command().scope()).await?;
    verbs::derive_append::validate_derived_origins_in_tx(
        &mut tx,
        &draft,
        &new_origins,
        &new_references,
    )
    .await?;
    verbs::derive_append::validate_derived_reference_kinds_in_tx(
        &mut tx,
        &draft,
        &new_origins,
        &new_references,
    )
    .await?;
    storage.platform_scope.bind_transaction(&mut tx).await?;
    source_reads
        .revalidate(
            storage,
            &mut tx,
            permit.command().scope(),
            &roles,
            &endpoints,
        )
        .await?;

    validate_goal_work_target(
        &mut tx,
        req,
        permit,
        &mut draft,
        goal,
        goal_owner,
        goal_generation,
    )
    .await?;
    let (outcome, edge_count) =
        admit_goal_work(storage, &mut tx, req, permit, &draft, goal, goal_owner).await?;
    // Re-read the exact source footprint after all admission work and before
    // commit.  Expiry, revocation, publication, or source-head changes after
    // the first check therefore abort the whole transaction rather than
    // leaving a Goal work row admitted on stale authority.
    source_reads
        .revalidate(
            storage,
            &mut tx,
            permit.command().scope(),
            &roles,
            &endpoints,
        )
        .await?;
    if !crate::ports::goal_collaboration::current_goal_role(
        &mut tx,
        permit.command(),
        &roles,
        goal_generation,
    )
    .await?
    .is_some_and(|role| role.may_write(permit.kind()))
    {
        return Err(StorageError::NotFound);
    }
    tx.commit().await.map_err(crate::error::map_err)?;
    Ok(AuthorDerivedOutcome {
        memory_id: outcome.memory_id,
        idempotent_replay: outcome.idempotent_replay,
        forgotten: outcome.forgotten,
        edge_count,
        embedding_deferred: !req.queued_spaces.is_empty() && !outcome.idempotent_replay,
    })
}
