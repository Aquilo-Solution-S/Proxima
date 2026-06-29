use std::sync::Arc;

use crate::common::{drop_db, fresh_pg};
use proxima_core::engine::{
    AuthorDerivedAuthorizedOutcome, AuthorDerivedEdgeInput, AuthorDerivedRequestInput, Engine,
};
use proxima_core::{
    AbstractionPayload, AgentDerivationV1, AgentNoteV1, AuthPath, AuthzContext,
    CORE_DERIVED_FROM_RELATION, EdgeAuthorshipKind, EntityKind, ErrorCode, FlavorRegistry, GroupId,
    MemoryId, MemoryOperatorKind, Owner, OwnerRef, Relation, Role, SchemaId, SchemaVersion,
    SidecarPayload, SourceBatchId, UserId,
};
use uuid::Uuid;

type ResolvedAuthz = AuthzContext;

#[tokio::test]
async fn cross_owner_derived_edge_requires_source_write_and_target_read()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let engine = Engine::new(FlavorRegistry::new().freeze())
            .with_storage_ports(Arc::new(pg.clone()).storage_ports());

        let p = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let no_read = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let gp = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let g1 = OwnerRef::Group(GroupId::new(Uuid::now_v7()));

        seed_membership(&pg, &gp, &p, Relation::Editor).await?;
        seed_membership(&pg, &g1, &p, Relation::Viewer).await?;
        seed_membership(&pg, &gp, &no_read, Relation::Editor).await?;

        let target = ingest_note_fact(&engine, &g1, "g1 target").await?;

        let ok = author_abstraction_over_target(
            &engine,
            &user_authz_with_roles(&p, [(gp, Role::editor()), (g1, Role::viewer())]),
            gp,
            target,
            "readable target",
        )
        .await?;
        assert_eq!(ok.edge_ids.len(), 1);
        assert_eq!(
            edge_change_event_owner(&pg, ok.edge_ids[0].into_inner()).await?,
            gp
        );

        let err = author_abstraction_over_target(
            &engine,
            &user_authz_with_roles(&no_read, [(gp, Role::editor())]),
            gp,
            target,
            "unreadable target",
        )
        .await
        .expect_err("target read access is required");
        assert_eq!(err.code, ErrorCode::Forbidden);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

async fn ingest_note_fact(
    engine: &Engine,
    owner: &Owner,
    body: &str,
) -> Result<MemoryId, proxima_core::ProtocolError> {
    let note = AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: body.to_string(),
        body: body.to_string(),
        tags: Vec::new(),
        idempotency_key: None,
    };
    let draft = proxima_core::verbs::fact_ingest::FactWriteCommand::from_payload(
        "edge-append-pg",
        SourceBatchId::new(Uuid::now_v7()),
        &note,
        time::OffsetDateTime::now_utc(),
    );
    let authz = match *owner {
        OwnerRef::Personal(subject) => AuthzContext::for_subject(subject, AuthPath::System),
        OwnerRef::Group(_) => AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(*owner, Role::admin())],
            AuthPath::System,
        )
        .narrowed_to_owner(*owner)
        .expect("group admin narrows to target owner"),
        OwnerRef::World => AuthzContext::denied_for_owner(owner),
    };
    engine
        .fact_ingest(&authz, draft)
        .await
        .map(|outcome| outcome.memory_id)
}

async fn author_abstraction_over_target(
    engine: &Engine,
    authz: &AuthzContext,
    source_owner: Owner,
    target: MemoryId,
    label: &str,
) -> Result<AuthorDerivedAuthorizedOutcome, proxima_core::ProtocolError> {
    let relation = engine
        .registry()
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .expect("core derived-from relation registered");
    let source = MemoryId::new(Uuid::now_v7());
    let edges = [AuthorDerivedEdgeInput {
        relation,
        source_kind: EntityKind::Abstraction,
        source_memory_id: source,
        target_kind: EntityKind::Fact,
        target_memory_id: target,
        authorship_kind: EdgeAuthorshipKind::ExternalAgent,
        authorship_owner_memory_id: None,
    }];

    engine
        .author_derived_authorized(
            authz,
            AuthorDerivedRequestInput {
                memory_id: source,
                owner: source_owner,
                kind: EntityKind::Abstraction,
                text: label.to_string(),
                schema_id: SchemaId::new(AgentDerivationV1::SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(AgentDerivationV1::SCHEMA_VERSION),
                operator_kind: MemoryOperatorKind::ExternalAgent,
                model_id: "test-model",
                prompt_version: "edge-append-pg",
                author_personality_instance_id: None,
                sidecar_payload: SidecarPayload::abstraction(AgentDerivationV1 {
                    title: label.to_string(),
                    body: label.to_string(),
                    tags: Vec::new(),
                    idempotency_key: None,
                    source_memory_ids: vec![target.into_inner()],
                    model_id: "test-model".to_string(),
                    client_name: "integration-test".to_string(),
                    client_version: "1".to_string(),
                }),
                supersedes: None,
                edges: &edges,
            },
        )
        .await
}

fn user_authz_with_roles<I>(user: &OwnerRef, roles: I) -> ResolvedAuthz
where
    I: IntoIterator<Item = (OwnerRef, Role)>,
{
    let OwnerRef::Personal(subject) = *user else {
        panic!("edge append test principal must be a user");
    };
    AuthzContext::for_subject_with_role(subject, roles, AuthPath::HostBearer)
}

async fn seed_membership(
    pg: &proxima_storage_pg::PgStorage,
    group: &OwnerRef,
    member: &OwnerRef,
    relation: Relation,
) -> Result<(), sqlx::Error> {
    let OwnerRef::Group(group_id) = group else {
        panic!("group principal required");
    };
    let OwnerRef::Personal(member_id) = member else {
        panic!("user principal required");
    };
    sqlx::query(
        "INSERT INTO proxima_core.group_memberships
            (group_id, member_user_id, relation)
         VALUES ($1, $2, $3::proxima_core.membership_relation)",
    )
    .bind(group_id.into_inner())
    .bind(member_id.into_inner())
    .bind(relation)
    .execute(pg.pool())
    .await?;
    Ok(())
}

async fn edge_change_event_owner(
    pg: &proxima_storage_pg::PgStorage,
    edge_id: Uuid,
) -> Result<OwnerRef, sqlx::Error> {
    let (kind, id): (proxima_core::OwnerRefKind, Option<Uuid>) = sqlx::query_as(
        "SELECT owner_kind, owner_id
           FROM proxima_core.change_event
          WHERE edge_id = $1
            AND kind = 'EdgeAppend'",
    )
    .bind(edge_id)
    .fetch_one(pg.pool())
    .await?;
    Ok(kind.with_uuid(id).expect("change_event owner_ref shape"))
}
