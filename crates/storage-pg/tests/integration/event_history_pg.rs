//! End-to-end `EventHistory` verb test against a transient PG database.

use crate::common::{
    create_db, db_url, drop_db, fresh_pg, seed_memory, seed_memory_edge, share_entity,
};
use std::collections::HashSet;
use std::sync::Arc;

use proxima_core::access::{AccessScope, world};
use proxima_core::engine::Engine;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_history::EventHistoryRequest;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::verbs::query::{
    EdgeFilter, EdgeReadRequest, PersonalityRootFilter, QueryRequest, SupersessionStatus,
    TombstoneFilter,
};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    AuthPath, AuthzContext, CORE_DERIVED_FROM_RELATION, CapabilitySet, ChangeEventKind, EdgeId,
    EntityKind, GroupId, Identity, MemoryId, Owner, Principal, RelationClass, SchemaId,
    SchemaVersion, SourceBatchId, SourceId, ToolScope, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

fn schemas_for_test() -> Vec<SchemaInfo> {
    vec![
        SchemaInfo {
            schema_id: SchemaId::new("test/fact_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Fact,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            has_typed_ingress: false,
            cited_object_schema: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/cited_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitedObject,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            has_typed_ingress: false,
            cited_object_schema: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/citation_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitationMapping,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            has_typed_ingress: false,
            cited_object_schema: None,
        },
    ]
}

fn fresh_event_draft(owner: Owner, payload: Vec<u8>) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner,
        author_personality_instance_id: None,
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload,
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("test/cited_blob".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: [42u8; 32],
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("test/citation_blob".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    }
}

fn build_engine(storage: Arc<dyn Storage>, _owner: Owner, _principal: Principal) -> Engine {
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    Engine::new(registry).with_storage(storage)
}

fn read_set_authz(
    principal: Principal,
    read_owners: impl IntoIterator<Item = Principal>,
) -> AuthzContext {
    AuthzContext {
        identity: Identity {
            principal,
            accessible_principals: read_owners.into_iter().collect::<HashSet<_>>(),
            expires_at: None,
            auth_epoch: 0,
        },
        capabilities: CapabilitySet {
            tool_scope: ToolScope::All,
            access: AccessScope::Unrestricted,
        },
        auth_path: AuthPath::System,
    }
}

#[tokio::test]
async fn event_history_returns_owner_scoped_newest_first() {
    let db_name = format!("proxima_test_eh_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let user1 = UserId::new(Uuid::now_v7());
        let user2 = UserId::new(Uuid::now_v7());
        let owner1 = Principal::User(user1);
        let owner2 = Principal::User(user2);
        let engine1 = build_engine(storage.clone(), owner1.clone(), Principal::User(user1));
        let engine2 = build_engine(storage, owner2.clone(), Principal::User(user2));
        let authz1 =
            proxima_core::AuthzContext::single_owner(&owner1, proxima_core::AuthPath::System);
        let authz2 =
            proxima_core::AuthzContext::single_owner(&owner2, proxima_core::AuthPath::System);

        for body in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
            engine1
                .event_ingest(&authz1, fresh_event_draft(owner1.clone(), body))
                .await?;
        }
        engine2
            .event_ingest(&authz2, fresh_event_draft(owner2.clone(), b"z".to_vec()))
            .await?;

        let resp1 = engine1
            .event_history(
                &authz1,
                &EventHistoryRequest {
                    principal: owner1.clone(),
                    limit: 100,
                    before: None,
                },
            )
            .await?;
        assert_eq!(resp1.events.len(), 3, "owner1 should see its events only");
        assert!(
            resp1.events[0].seq > resp1.events[1].seq && resp1.events[1].seq > resp1.events[2].seq,
            "events must be newest-first",
        );
        assert_eq!(
            resp1.seq_high_water,
            Some(resp1.events[0].seq),
            "seq_high_water is the newest event seq for that owner",
        );

        let resp2 = engine2
            .event_history(
                &authz2,
                &EventHistoryRequest {
                    principal: owner2.clone(),
                    limit: 100,
                    before: None,
                },
            )
            .await?;
        assert_eq!(resp2.events.len(), 1, "owner2 is isolated from owner1");

        let page1 = engine1
            .event_history(
                &authz1,
                &EventHistoryRequest {
                    principal: owner1.clone(),
                    limit: 2,
                    before: None,
                },
            )
            .await?;
        assert_eq!(page1.events.len(), 2);

        let page2 = engine1
            .event_history(
                &authz1,
                &EventHistoryRequest {
                    principal: owner1,
                    limit: 2,
                    before: Some(page1.events[1].seq),
                },
            )
            .await?;
        assert_eq!(page2.events.len(), 1, "oldest event remains");
        assert!(page2.events[0].seq < page1.events[1].seq);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("event_history_returns_owner_scoped_newest_first failed");
}

#[tokio::test]
async fn event_history_applies_public_source_guard_to_edge_events()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let gp = Principal::Group(GroupId::new(Uuid::now_v7()));
        let private = Principal::Group(GroupId::new(Uuid::now_v7()));
        let p = Principal::User(UserId::new(Uuid::now_v7()));
        let world = world();

        let a_public = seed_memory(&pg, &gp, EntityKind::Abstraction, "A public").await?;
        let f_private = seed_memory(&pg, &private, EntityKind::Fact, "private").await?;
        share_entity(&pg, a_public.into_inner(), &world).await?;

        let edge = seed_memory_edge(
            &pg,
            &gp,
            (EntityKind::Abstraction, a_public),
            (EntityKind::Fact, f_private),
            CORE_DERIVED_FROM_RELATION,
            RelationClass::Provenance,
        )
        .await?;
        insert_edge_append_event(&pg, &gp, edge, a_public, f_private).await?;

        let p_read = vec![p.clone(), gp.clone(), world.clone()];
        let read_edges = pg
            .read_edges(
                &p_read,
                &EdgeReadRequest {
                    principal: p.clone(),
                    edge_ids: vec![edge],
                    filter: EdgeFilter::default(),
                    limit: 10,
                },
            )
            .await?;
        assert!(
            read_edges.edges.is_empty(),
            "read_edges omits the public-source/private-target edge"
        );

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());
        let engine = build_engine(storage, p.clone(), p.clone());
        let authz = read_set_authz(p, p_read);
        let history = engine
            .event_history(
                &authz,
                &EventHistoryRequest {
                    principal: private,
                    limit: 100,
                    before: None,
                },
            )
            .await?;

        assert!(
            !history.events.iter().any(|e| matches!(
                &e.kind,
                ChangeEventKind::EdgeAppend { edge_id, .. } if *edge_id == edge.into_inner()
            )),
            "event_history must not disclose an edge hidden by read_edges"
        );
        assert!(
            history.seq_high_water.is_none(),
            "high-water must be computed over visible events only"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn query_high_water_applies_public_source_guard_to_edge_events()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let gp = Principal::Group(GroupId::new(Uuid::now_v7()));
        let private = Principal::Group(GroupId::new(Uuid::now_v7()));
        let p = Principal::User(UserId::new(Uuid::now_v7()));
        let world = world();

        let a_public = seed_memory(&pg, &gp, EntityKind::Abstraction, "A public").await?;
        let f_private = seed_memory(&pg, &private, EntityKind::Fact, "private").await?;
        share_entity(&pg, a_public.into_inner(), &world).await?;

        let edge = seed_memory_edge(
            &pg,
            &gp,
            (EntityKind::Abstraction, a_public),
            (EntityKind::Fact, f_private),
            CORE_DERIVED_FROM_RELATION,
            RelationClass::Provenance,
        )
        .await?;
        let hidden_seq = insert_edge_append_event(&pg, &gp, edge, a_public, f_private).await?;

        let p_read = vec![p.clone(), gp, world];
        let query = pg
            .query_memories(
                &QueryRequest {
                    principal: p,
                    read_owners: p_read,
                    entity_kind: None,
                    schema_id: None,
                    supersession: SupersessionStatus::HeadsOnly,
                    tombstones: TombstoneFilter::PresentOnly,
                    personality_roots: PersonalityRootFilter::IncludeInactive,
                    limit: 10,
                    include_payloads: false,
                    memory_ids: Vec::new(),
                    goal_ids: Vec::new(),
                    edge_ids: Vec::new(),
                    stateful_heads: Vec::new(),
                    reader_personality_instance_id: None,
                },
                &[],
            )
            .await?;

        assert_ne!(
            query.seq_high_water,
            Some(hidden_seq),
            "query high-water must not expose a hidden edge event seq"
        );
        assert!(
            query.seq_high_water.is_none(),
            "with only a hidden readable-owner edge event, query high-water has no visible seq"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

async fn insert_edge_append_event(
    pg: &PgStorage,
    owner: &Principal,
    edge_id: EdgeId,
    source_memory_id: MemoryId,
    target_memory_id: MemoryId,
) -> Result<Uuid, sqlx::Error> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let seq = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event \
            (seq, owner_principal_kind, owner_principal_id, kind, \
             edge_id, edge_relation, \
             edge_source_memory_id, edge_source_goal_id, edge_source_fact_entity_id, \
             edge_target_memory_id, edge_target_goal_id, edge_target_fact_entity_id) \
         VALUES ($1, $2, $3, 'EdgeAppend', $4, $5, $6, NULL, NULL, $7, NULL, NULL)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(edge_id.into_inner())
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(source_memory_id.into_inner())
    .bind(target_memory_id.into_inner())
    .execute(pg.pool())
    .await
    .map(|_| seq)
}
