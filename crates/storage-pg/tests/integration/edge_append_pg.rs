use std::collections::HashSet;
use std::sync::Arc;

use crate::common::{drop_db, fresh_pg};
use proxima_core::engine::{
    AuthorDerivedAuthorizedOutcome, AuthorDerivedEdgeInput, AuthorDerivedRequestInput, Engine,
};
use proxima_core::storage::Storage;
use proxima_core::{
    AbstractionPayload, AccessScope, AgentDerivationV1, AgentNoteV1, AuthPath, AuthzContext,
    CORE_DERIVED_FROM_RELATION, CapabilitySet, EdgeAuthorshipKind, EntityKind, ErrorCode,
    FlavorRegistry, GroupId, Identity, MemoryId, MemoryOperatorKind, Owner, Principal, Relation,
    SchemaId, SchemaVersion, SidecarPayload, SourceBatchId, ToolScope, UserId,
};
use uuid::Uuid;

#[tokio::test]
async fn cross_owner_derived_edge_requires_source_write_and_target_read()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let engine = Engine::new(FlavorRegistry::new().freeze()).with_storage(storage);

        let p = Principal::User(UserId::new(Uuid::now_v7()));
        let no_read = Principal::User(UserId::new(Uuid::now_v7()));
        let gp = Principal::Group(GroupId::new(Uuid::now_v7()));
        let g1 = Principal::Group(GroupId::new(Uuid::now_v7()));

        seed_membership(&pg, &gp, &p, Relation::Editor).await?;
        seed_membership(&pg, &g1, &p, Relation::Viewer).await?;
        seed_membership(&pg, &gp, &no_read, Relation::Editor).await?;

        let target = ingest_note_fact(&engine, &g1, "g1 target").await?;

        let ok = author_abstraction_over_target(
            &engine,
            &granted_user_authz(&p),
            gp.clone(),
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
            &granted_user_authz(&no_read),
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
    let draft = proxima_core::verbs::event_ingest::EventDraft::from_payload(
        owner,
        "edge-append-pg",
        SourceBatchId::new(Uuid::now_v7()),
        &note,
        time::OffsetDateTime::now_utc(),
    );
    engine
        .event_ingest(&AuthzContext::single_owner(owner, AuthPath::System), draft)
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

fn granted_user_authz(user: &Principal) -> AuthzContext {
    let mut accessible_principals = HashSet::new();
    accessible_principals.insert(user.clone());
    AuthzContext {
        identity: Identity {
            principal: user.clone(),
            accessible_principals,
            expires_at: None,
            auth_epoch: 0,
        },
        capabilities: CapabilitySet {
            tool_scope: ToolScope::All,
            access: AccessScope::Granted,
        },
        auth_path: AuthPath::HostBearer,
    }
}

async fn seed_membership(
    pg: &proxima_storage_pg::PgStorage,
    group: &Principal,
    member: &Principal,
    relation: Relation,
) -> Result<(), sqlx::Error> {
    let Principal::Group(group_id) = group else {
        panic!("group principal required");
    };
    let Principal::User(member_id) = member else {
        panic!("user principal required");
    };
    sqlx::query(
        "INSERT INTO proxima_core.group_membership
            (group_id, member_user_id, relation, granted_by)
         VALUES ($1, $2, $3::proxima_core.membership_relation, $4)",
    )
    .bind(group_id.into_inner())
    .bind(member_id.into_inner())
    .bind(relation)
    .bind(Uuid::nil())
    .execute(pg.pool())
    .await?;
    Ok(())
}

async fn edge_change_event_owner(
    pg: &proxima_storage_pg::PgStorage,
    edge_id: Uuid,
) -> Result<Principal, sqlx::Error> {
    let (kind, id): (proxima_core::OwnerPrincipalKind, Uuid) = sqlx::query_as(
        "SELECT owner_principal_kind, owner_principal_id
           FROM proxima_core.change_event
          WHERE edge_id = $1
            AND kind = 'EdgeAppend'",
    )
    .bind(edge_id)
    .fetch_one(pg.pool())
    .await?;
    Ok(kind.with_uuid(id))
}
