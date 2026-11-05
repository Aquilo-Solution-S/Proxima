//! Independent typed A/P copies, under exact source-use and destination authority.

use proxiima_core::verbs::fact_ingest::FactWriteCommand;
use proxiima_core::verbs::idempotency::{ContentFingerprint, KeyScope, RequestKey};
use proxiima_core::{
    AccessKind, EdgeEndpoint, EntityKind, ForkCommandPermit, ForkDestinationPermit,
    ForkMemoryOutcome, ForkMemoryRequest, GoalHandle, MemoryId, MemoryKind, OwnerId, StorageError,
};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::access::owner_columns::CitedBlobUploads;
use crate::ports::goal_collaboration::source_reads::{SourceReads, lock_goal_footprint};
use crate::ports::goal_collaboration::{
    current_goal_role, current_role, fenced_roles, role_for_goal, source_admin,
};
use crate::{PgStorage, error::map_err};

struct ForkSource {
    source_owner: OwnerId,
    cited_blob: Option<Uuid>,
    memory_kind: MemoryKind,
    entity_kind: EntityKind,
    schema_id: String,
    payload: proxiima_core::SidecarPayload,
    lexical_language: Option<String>,
    consent: Option<(GoalHandle, OwnerId)>,
    source_endpoint: [EdgeEndpoint; 1],
    origins: Vec<EdgeEndpoint>,
    references: Vec<EdgeEndpoint>,
    external_origins: Vec<Uuid>,
    external_references: Vec<Uuid>,
}

struct ForkAuthority {
    reads: SourceReads,
    roles: proxiima_core::OwnerRoles,
    same_owner_destination: bool,
    cited_uploads: CitedBlobUploads,
}

struct ForkAdmission<'a> {
    source_owner: OwnerId,
    target_owner: OwnerId,
    cited_blob: Option<Uuid>,
    cited_uploads: CitedBlobUploads,
    memory_kind: MemoryKind,
    schema_id: &'a str,
    draft: &'a FactWriteCommand,
    origins: &'a [EdgeEndpoint],
    references: &'a [EdgeEndpoint],
    payload: &'a proxiima_core::SidecarPayload,
}

async fn load_fork_source(
    storage: &PgStorage,
    tx: &mut Transaction<'_, Postgres>,
    request: &ForkMemoryRequest,
    permit: &ForkCommandPermit,
    target_owner: OwnerId,
) -> Result<ForkSource, StorageError> {
    crate::owner_scope::bind_owner_scope(tx, permit.scope()).await?;
    let source = sqlx::query("SELECT m.kind::text,m.owner_id,m.schema_id,m.origins,m.refs,m.goal_refs,m.lexical_language::text,m.blob_id
        FROM proxiima_core.memory m /* FORGET-POLICY: live-only (exact fork source) */ WHERE m.t=$1 AND m.forgotten_at IS NULL")
        .bind(request.source.into_inner())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?
        .ok_or(StorageError::NotFound)?;
    let source_owner = OwnerId::new(source.get("owner_id"));
    if source_owner != permit.source_owner() {
        return Err(StorageError::Retryable("fork source owner changed".into()));
    }
    let (memory_kind, entity_kind) = kind(source.get::<String, _>("kind").as_str())?;
    if memory_kind == MemoryKind::Fact {
        return Err(StorageError::ConstraintViolation(
            "Facts are immutable admissions, not A/P forks".into(),
        ));
    }
    let schema_id: String = source.get("schema_id");
    if target_owner != source_owner
        && let Some(why) = storage.surfaces.untransferable_schema(&schema_id)
    {
        return Err(StorageError::ConstraintViolation(format!(
            "schema cannot leave its owner: {why}"
        )));
    }
    let payload_kind = match memory_kind {
        MemoryKind::Abstraction => proxiima_core::verbs::schema::PayloadKind::Abstraction,
        MemoryKind::Perspective => proxiima_core::verbs::schema::PayloadKind::Perspective,
        MemoryKind::Fact => unreachable!(),
    };
    let sidecar_key = storage
        .sidecars
        .memory_key_for(payload_kind, &schema_id)
        .ok_or(StorageError::NotFound)?;
    let payload = storage
        .sidecars
        .load_fork_payload((&mut **tx).into(), sidecar_key, request.source)
        .await?;
    let routes: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT DISTINCT goal_handle,controller_owner FROM proxiima_core.goal_memory_read_routes() WHERE memory_t=$1 ORDER BY goal_handle",
    )
    .bind(request.source.into_inner())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    storage.platform_scope.bind_transaction(tx).await?;
    let route_ids: Vec<_> = routes.iter().map(|(goal, _)| *goal).collect();
    let fork_route: Option<Uuid> = sqlx::query_scalar("SELECT p.goal_handle FROM proxiima_core.goal_memory_publication p
        JOIN proxiima_core.goal_head gh ON gh.handle=p.goal_handle AND gh.authority_generation=p.controller_generation
        JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (fork consent under owner RLS) */ ON m.t=p.memory_t AND m.owner_id=p.source_owner AND m.forgotten_at IS NULL
        JOIN proxiima_core.memory_head mh ON mh.handle=m.handle AND mh.owner_id=p.source_owner AND mh.authority_generation=p.source_generation
        WHERE p.memory_t=$1 AND p.goal_handle=ANY($2::uuid[]) AND p.permitted_use='fork' ORDER BY p.goal_handle LIMIT 1")
        .bind(request.source.into_inner()).bind(&route_ids).fetch_optional(&mut **tx).await.map_err(map_err)?;
    let consent = fork_route
        .and_then(|goal| routes.iter().find(|(id, _)| *id == goal))
        .map(|(goal, owner)| (GoalHandle::new(*goal), OwnerId::new(*owner)));
    let origin_ids: Vec<Uuid> = source.get("origins");
    let ref_ids: Vec<Uuid> = source.get("refs");
    let goal_ids: Vec<Uuid> = source.get("goal_refs");
    let origins = copied_memory_endpoints(tx, &origin_ids).await?;
    let mut references = copied_memory_endpoints(tx, &ref_ids).await?;
    references.extend(
        goal_ids
            .into_iter()
            .map(|id| EdgeEndpoint::goal(proxiima_core::GoalId::new(id))),
    );
    let external_origins: Vec<Uuid> = sqlx::query_scalar("SELECT memory FROM proxiima_core.memory_external_origin WHERE t=$1 AND edge_kind='origin' ORDER BY memory").bind(request.source.into_inner()).fetch_all(&mut **tx).await.map_err(map_err)?;
    let external_references: Vec<Uuid> = sqlx::query_scalar("SELECT memory FROM proxiima_core.memory_external_origin WHERE t=$1 AND edge_kind='reference' ORDER BY memory").bind(request.source.into_inner()).fetch_all(&mut **tx).await.map_err(map_err)?;
    Ok(ForkSource {
        source_owner,
        cited_blob: source.get("blob_id"),
        memory_kind,
        entity_kind,
        schema_id,
        payload,
        lexical_language: source.get("lexical_language"),
        consent,
        source_endpoint: [EdgeEndpoint::memory(entity_kind, request.source)],
        origins,
        references,
        external_origins,
        external_references,
    })
}

async fn authorize_fork(
    storage: &PgStorage,
    tx: &mut Transaction<'_, Postgres>,
    permit: &ForkCommandPermit,
    source: &ForkSource,
    target_owner: OwnerId,
) -> Result<ForkAuthority, StorageError> {
    let reads = SourceReads::discover(storage, tx, permit.scope(), &source.source_endpoint).await?;
    let mut owners: Vec<_> = reads
        .owners()
        .chain([source.source_owner, target_owner])
        .collect();
    owners.extend(source.consent.map(|(_, owner)| owner));
    let roles = fenced_roles(storage, tx, permit.scope(), &owners, &[]).await?;
    let mut goals: Vec<_> = reads.goal_routes().chain(source.consent).collect();
    if let ForkDestinationPermit::WorkingGoal(command) = permit.destination() {
        goals.push((command.goal(), command.controller()));
    }
    lock_goal_footprint(tx, goals).await?;
    reads
        .revalidate(storage, tx, permit.scope(), &roles, &source.source_endpoint)
        .await?;
    check_destination(tx, permit, &roles, AccessKind::from(source.entity_kind)).await?;
    let same_owner_destination = matches!(permit.destination(), ForkDestinationPermit::Owner(_))
        && target_owner == source.source_owner;
    let cited_uploads =
        if !same_owner_destination && !source_admin(permit.scope(), &roles, source.source_owner) {
            CitedBlobUploads::Publication(
                check_fork_consent(
                    tx,
                    permit,
                    &roles,
                    source.consent,
                    AccessKind::from(source.entity_kind),
                )
                .await?,
            )
        } else {
            CitedBlobUploads::Owner
        };
    Ok(ForkAuthority {
        reads,
        roles,
        same_owner_destination,
        cited_uploads,
    })
}

async fn admit_fork(
    storage: &PgStorage,
    tx: &mut Transaction<'_, Postgres>,
    request: &ForkMemoryRequest,
    permit: &ForkCommandPermit,
    admission: ForkAdmission<'_>,
) -> Result<proxiima_core::verbs::fact_ingest::FactIngestOutcome, StorageError> {
    let ForkAdmission {
        source_owner,
        target_owner,
        cited_blob,
        cited_uploads,
        memory_kind,
        schema_id,
        draft,
        origins,
        references,
        payload,
    } = admission;
    let payloads = [payload.clone()];
    let tables = storage.sidecars.tables_for_payloads(&payloads)?;
    let scopes = storage.scopes.targets_for_payloads(&payloads)?;
    let identity = proxiima_core::new_memory_uuid(storage.store_number);
    let prepared = super::memory_timeseries::prepare_memory_admission_at(
        tx,
        super::memory_timeseries::MemoryAdmissionDraft {
            store_number: storage.store_number,
            natural_key: None,
            owner: &target_owner,
            writer: permit.scope().writer(),
            draft,
            origins,
            references,
            sidecar_tables: &tables,
            scopes: &scopes,
        },
        super::memory_timeseries::PreparationOptions {
            identity: Some((identity, identity)),
            extra_targets: &[request.source.into_inner()],
            guided_by: &[],
            forked_from: Some(request.source.into_inner()),
        },
    )
    .await?;
    super::memory_timeseries::lock_prepared_memory_admission(tx, &prepared).await?;
    let prepared = super::memory_timeseries::claim_prepared_memory_admission(tx, prepared).await?;
    let outcome = match prepared {
        super::memory_timeseries::PreparedMemoryAdmission::Replay(outcome) => outcome,
        super::memory_timeseries::PreparedMemoryAdmission::New(prepared) => {
            let blob_id = match cited_blob {
                Some(blob_id) => Some(
                    crate::access::owner_columns::copy_cited_blob_for_owner(
                        tx,
                        &storage.surfaces,
                        source_owner.into_inner(),
                        target_owner.into_inner(),
                        blob_id,
                        cited_uploads,
                    )
                    .await?,
                ),
                None => None,
            };
            let content_row = super::content::ensure_content_from_payloads(
                tx,
                target_owner.into_inner(),
                memory_kind.as_str(),
                schema_id,
                &payloads,
            )
            .await?;
            let outcome =
                super::memory_timeseries::materialize_prepared_memory_admission_after_locks(
                    tx,
                    prepared,
                    blob_id,
                    content_row,
                )
                .await?;
            storage
                .sidecars
                .writing(draft)
                .insert_memory_sidecar(tx, outcome.memory_id, payload)
                .await?;
            super::fact_embeddings::enqueue_embedding_jobs_in_tx(
                tx,
                target_owner.into_inner(),
                outcome.memory_id.into_inner(),
                permit.queued_spaces(),
            )
            .await?;
            if let ForkDestinationPermit::WorkingGoal(command) = permit.destination() {
                sqlx::query("INSERT INTO proxiima_core.goal_work_binding(goal_handle,memory_handle,source_owner,source_generation,controller_generation,bound_by)
                    SELECT $1,mh.handle,mh.owner_id,mh.authority_generation,gh.authority_generation,$3
                    FROM proxiima_core.memory_head mh JOIN proxiima_core.goal_head gh ON gh.handle=$1 WHERE mh.handle=$2 AND mh.owner_id=gh.owner_id")
                    .bind(command.goal().into_inner()).bind(outcome.handle).bind(permit.scope().subject().into_inner()).execute(&mut **tx).await.map_err(map_err)?;
            }
            outcome
        }
    };
    Ok(outcome)
}

fn kind(raw: &str) -> Result<(MemoryKind, EntityKind), StorageError> {
    match raw {
        "fact" => Ok((MemoryKind::Fact, EntityKind::Fact)),
        "abstraction" => Ok((MemoryKind::Abstraction, EntityKind::Abstraction)),
        "perspective" => Ok((MemoryKind::Perspective, EntityKind::Perspective)),
        _ => Err(StorageError::ConstraintViolation(
            "unknown memory kind".into(),
        )),
    }
}

fn fork_request_key(
    request: &ForkMemoryRequest,
    permit: &ForkCommandPermit,
    target_owner: OwnerId,
    memory_kind: MemoryKind,
) -> RequestKey {
    let key_scope = match (permit.destination(), memory_kind) {
        (ForkDestinationPermit::WorkingGoal(command), MemoryKind::Abstraction) => {
            KeyScope::GoalAbstraction(command.goal(), permit.scope().subject())
        }
        (ForkDestinationPermit::WorkingGoal(command), MemoryKind::Perspective) => {
            KeyScope::GoalPerspective(command.goal(), permit.scope().subject())
        }
        (ForkDestinationPermit::Owner(_), MemoryKind::Abstraction) => KeyScope::Abstraction,
        (ForkDestinationPermit::Owner(_), MemoryKind::Perspective) => KeyScope::Perspective,
        (_, MemoryKind::Fact) => unreachable!(),
    };
    let destination_goal = match permit.destination() {
        ForkDestinationPermit::Owner(_) => Vec::new(),
        ForkDestinationPermit::WorkingGoal(command) => vec![command.goal().into_inner()],
    };
    let fingerprint = ContentFingerprint::builder(key_scope)
        .text("operation", "fork")
        .uuid_set("source", [request.source.into_inner()])
        .uuid_set("destination_owner", [target_owner.into_inner()])
        .uuid_set("destination_goal", destination_goal)
        .build();
    RequestKey::new(key_scope, request.request_id.clone(), fingerprint)
}

pub(crate) async fn fork_memory(
    storage: &PgStorage,
    request: &ForkMemoryRequest,
    permit: &ForkCommandPermit,
) -> Result<ForkMemoryOutcome, StorageError> {
    for attempt in 0..3 {
        match fork_once(storage, request, permit).await {
            Err(StorageError::Retryable(_)) if attempt < 2 => {}
            outcome => return outcome,
        }
    }
    unreachable!()
}

async fn fork_once(
    storage: &PgStorage,
    request: &ForkMemoryRequest,
    permit: &ForkCommandPermit,
) -> Result<ForkMemoryOutcome, StorageError> {
    if request.source != permit.source() {
        return Err(StorageError::NotFound);
    }
    let target_owner = match permit.destination() {
        ForkDestinationPermit::Owner(write) => *write.owner(),
        ForkDestinationPermit::WorkingGoal(command) => command.controller(),
    };
    let mut tx = storage.platform_transaction().await?;
    let source = load_fork_source(storage, &mut tx, request, permit, target_owner).await?;
    let authority = authorize_fork(storage, &mut tx, permit, &source, target_owner).await?;
    let source_owner = source.source_owner;
    let cited_blob = source.cited_blob;
    let memory_kind = source.memory_kind;
    let entity_kind = source.entity_kind;
    let schema_id = source.schema_id.clone();
    let payload = source.payload.clone();
    let consent = source.consent;
    let source_endpoint = source.source_endpoint;
    let reads = authority.reads;
    let roles = authority.roles;
    let same_owner_destination = authority.same_owner_destination;
    let cited_uploads = authority.cited_uploads;
    let access_kind = AccessKind::from(entity_kind);
    let (origins, references, external_origins, external_references) = (
        source.origins,
        source.references,
        source.external_origins,
        source.external_references,
    );
    let request_key = fork_request_key(request, permit, target_owner, memory_kind);
    let text = super::sketch::sketch_line(entity_kind.into(), None, std::slice::from_ref(&payload));
    let draft = FactWriteCommand {
        schema_id: payload.schema_id.clone(),
        schema_version: payload.schema_version,
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: Some(text),
        lexical_language: source.lexical_language,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: memory_kind,
        request: Some(request_key),
    };
    let outcome = admit_fork(
        storage,
        &mut tx,
        request,
        permit,
        ForkAdmission {
            source_owner,
            target_owner,
            cited_blob,
            cited_uploads,
            memory_kind,
            schema_id: &schema_id,
            draft: &draft,
            origins: &origins,
            references: &references,
            payload: &payload,
        },
    )
    .await?;
    super::derive_append::record_external_edges(
        &mut tx,
        &super::derive_append::DerivedOutcome::from(outcome.clone()),
        &external_origins,
        &external_references,
    )
    .await?;
    reads
        .revalidate(storage, &mut tx, permit.scope(), &roles, &source_endpoint)
        .await?;
    check_destination(&mut tx, permit, &roles, access_kind).await?;
    if !same_owner_destination && !source_admin(permit.scope(), &roles, source_owner) {
        check_fork_consent(&mut tx, permit, &roles, consent, access_kind).await?;
    }
    tx.commit().await.map_err(map_err)?;
    Ok(ForkMemoryOutcome {
        memory_id: outcome.memory_id,
        handle: proxiima_core::SeriesHandle::new(outcome.handle),
        forked_from: request.source,
        idempotent_replay: outcome.idempotent_replay,
    })
}

async fn check_destination(
    tx: &mut Transaction<'_, Postgres>,
    permit: &ForkCommandPermit,
    roles: &proxiima_core::OwnerRoles,
    kind: AccessKind,
) -> Result<(), StorageError> {
    let role = match permit.destination() {
        ForkDestinationPermit::Owner(write) => current_role(permit.scope(), roles, *write.owner()),
        ForkDestinationPermit::WorkingGoal(command) => {
            let row = sqlx::query("SELECT gh.authority_generation FROM proxiima_core.goal_head gh JOIN proxiima_core.goal g ON g.t=gh.t AND g.handle=gh.handle WHERE gh.handle=$1 AND gh.owner_id=$2 AND g.state='Active'")
                .bind(command.goal().into_inner()).bind(command.controller().into_inner()).fetch_optional(&mut **tx).await.map_err(map_err)?.ok_or(StorageError::NotFound)?;
            current_goal_role(tx, command, roles, row.get("authority_generation")).await?
        }
    };
    if permit.scope().is_expired() || !role.is_some_and(|role| role.may_write(kind)) {
        return Err(StorageError::NotFound);
    }
    Ok(())
}

async fn check_fork_consent(
    tx: &mut Transaction<'_, Postgres>,
    permit: &ForkCommandPermit,
    roles: &proxiima_core::OwnerRoles,
    consent: Option<(GoalHandle, OwnerId)>,
    kind: AccessKind,
) -> Result<Option<Uuid>, StorageError> {
    let (goal, controller) = consent.ok_or(StorageError::NotFound)?;
    let publication = sqlx::query("SELECT gh.authority_generation,p.cited_upload_id FROM proxiima_core.goal_memory_publication p
        JOIN proxiima_core.goal_head gh ON gh.handle=p.goal_handle AND gh.owner_id=$3 AND gh.authority_generation=p.controller_generation
        JOIN proxiima_core.memory m /* FORGET-POLICY: live-only (fork consent under owner RLS) */ ON m.t=p.memory_t AND m.owner_id=p.source_owner AND m.forgotten_at IS NULL
        JOIN proxiima_core.memory_head mh ON mh.handle=m.handle AND mh.owner_id=p.source_owner AND mh.authority_generation=p.source_generation
        WHERE p.goal_handle=$1 AND p.memory_t=$2 AND p.source_owner=$4 AND p.permitted_use='fork'")
        .bind(goal.into_inner()).bind(permit.source().into_inner()).bind(controller.into_inner()).bind(permit.source_owner().into_inner()).fetch_optional(&mut **tx).await.map_err(map_err)?
        .ok_or(StorageError::NotFound)?;
    let role = role_for_goal(
        tx,
        permit.scope(),
        roles,
        goal,
        controller,
        publication.get("authority_generation"),
    )
    .await?;
    if !role.is_some_and(|role| role.may_read(kind)) {
        return Err(StorageError::NotFound);
    }
    Ok(publication.get("cited_upload_id"))
}

async fn copied_memory_endpoints(
    tx: &mut Transaction<'_, Postgres>,
    ids: &[Uuid],
) -> Result<Vec<EdgeEndpoint>, StorageError> {
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT t,kind::text FROM proxiima_core.memory /* FORGET-POLICY: includes-forgotten (copy the readable source declaration, without ancestor hydration) */ WHERE t=ANY($1::uuid[])")
            .bind(ids)
            .fetch_all(&mut **tx)
            .await
            .map_err(map_err)?;
    ids.iter()
        .map(|id| {
            let (_, raw) = rows
                .iter()
                .find(|(t, _)| t == id)
                .ok_or(StorageError::NotFound)?;
            Ok(EdgeEndpoint::memory(kind(raw)?.1, MemoryId::new(*id)))
        })
        .collect()
}
