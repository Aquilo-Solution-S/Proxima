//! End-to-end `ChangeHistory` verb test against a transient PG database.

use crate::common::{create_db, db_url, drop_db, fresh_pg, seed_memory, seed_memory_edge};
use std::sync::Arc;

use proxima_core::engine::{Engine, ListChangeEventsReadRequest};
use proxima_core::storage_ports::*;
use proxima_core::verbs::change_history::ChangeHistoryRequest;
use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::verbs::query::{
    EdgeFilter, EdgeReadRequest, EdgeTargetProjection, QueryRequest, SupersessionStatus,
    TombstoneFilter,
};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    AuthPath, AuthzContext, CORE_DERIVED_FROM_RELATION, ChangeEventKind, EdgeId, EntityKind,
    GroupId, MemoryId, Owner, OwnerRef, RelationClass, Role, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

type ResolvedAuthz = AuthzContext;

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

fn fresh_fact_draft(_owner: Owner, payload: Vec<u8>) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload,
        rendered_text: None,
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/source"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
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

fn build_engine(storage: StoragePorts, _owner: Owner, _principal: OwnerRef) -> Engine {
    let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
    Engine::new(registry).with_storage_ports(storage)
}

fn read_set_authz(
    principal: OwnerRef,
    read_owners: impl IntoIterator<Item = OwnerRef>,
) -> ResolvedAuthz {
    let OwnerRef::Personal(user) = principal else {
        panic!("event-history test principal must be a user");
    };
    let roles = read_owners
        .into_iter()
        .filter(|owner| matches!(owner, OwnerRef::Group(_)))
        .map(|owner| (owner, Role::viewer()));
    AuthzContext::for_subject_with_role(user, roles, AuthPath::HostBearer)
}

#[tokio::test]
async fn change_history_returns_owner_scoped_newest_first() {
    let db_name = format!("proxima_test_eh_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        let storage = Arc::new(pg.clone()).storage_ports();
        let user1 = UserId::new(Uuid::now_v7());
        let user2 = UserId::new(Uuid::now_v7());
        let owner1 = OwnerRef::Personal(user1);
        let owner2 = OwnerRef::Personal(user2);
        let engine1 = build_engine(storage.clone(), owner1, OwnerRef::Personal(user1));
        let engine2 = build_engine(storage, owner2, OwnerRef::Personal(user2));
        let authz1 =
            proxima_core::AuthzContext::single_owner(&owner1, proxima_core::AuthPath::HostBearer);
        let authz2 =
            proxima_core::AuthzContext::single_owner(&owner2, proxima_core::AuthPath::HostBearer);

        for body in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
            engine1
                .fact_ingest(&authz1, fresh_fact_draft(owner1, body))
                .await?;
        }
        engine2
            .fact_ingest(&authz2, fresh_fact_draft(owner2, b"z".to_vec()))
            .await?;

        let resp1 = engine1
            .change_history(
                &authz1,
                &ChangeHistoryRequest {
                    owner: owner1,
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
            .change_history(
                &authz2,
                &ChangeHistoryRequest {
                    owner: owner2,
                    limit: 100,
                    before: None,
                },
            )
            .await?;
        assert_eq!(resp2.events.len(), 1, "owner2 is isolated from owner1");

        let page1 = engine1
            .change_history(
                &authz1,
                &ChangeHistoryRequest {
                    owner: owner1,
                    limit: 2,
                    before: None,
                },
            )
            .await?;
        assert_eq!(page1.events.len(), 2);

        let page2 = engine1
            .change_history(
                &authz1,
                &ChangeHistoryRequest {
                    owner: owner1,
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
    result.expect("change_history_returns_owner_scoped_newest_first failed");
}

#[tokio::test]
async fn change_history_surfaces_readable_non_world_source_edge_events()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let gp = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let private = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let p = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let a_source = seed_memory(&pg, &gp, EntityKind::Abstraction, "A source").await?;
        let f_private = seed_memory(&pg, &private, EntityKind::Fact, "private").await?;

        let edge = seed_memory_edge(
            &pg,
            &gp,
            (EntityKind::Abstraction, a_source),
            (EntityKind::Fact, f_private),
            CORE_DERIVED_FROM_RELATION,
            RelationClass::Provenance,
        )
        .await?;
        insert_edge_append_event(&pg, &gp, edge, a_source, f_private).await?;

        let p_read = vec![p, gp];
        let read_edges = pg
            .read_edges(
                &p_read,
                &EdgeReadRequest {
                    owner: p,
                    edge_ids: vec![edge],
                    filter: EdgeFilter::default(),
                    limit: 10,
                    cursor: None,
                    include_payloads: false,
                },
                &[],
            )
            .await?;
        assert_eq!(read_edges.edges.len(), 1);
        assert_eq!(
            read_edges.edges[0].target,
            EdgeTargetProjection::Redacted,
            "read_edges keeps source-owned edge but redacts unreadable target"
        );

        let storage = Arc::new(pg.clone()).storage_ports();
        let engine = build_engine(storage, p, p);
        let authz = read_set_authz(p, p_read);
        let history = engine
            .change_history(
                &authz,
                &ChangeHistoryRequest {
                    owner: gp,
                    limit: 100,
                    before: None,
                },
            )
            .await?;

        assert!(
            history.events.iter().any(|e| matches!(
                &e.kind,
                ChangeEventKind::EdgeAppend { edge_id, target, .. }
                    if *edge_id == edge.into_inner() && *target == EdgeTargetProjection::Redacted
            )),
            "change_history surfaces source-owned non-world edge events with redacted target"
        );
        assert!(
            history.seq_high_water.is_some(),
            "high-water is computed over visible source-owned events"
        );

        let listed = engine
            .list_change_events(
                &authz,
                &ListChangeEventsReadRequest {
                    after: Uuid::nil(),
                    limit: 100,
                },
            )
            .await?;
        let endpoint_kind = listed
            .edge_endpoint_kinds
            .iter()
            .find(|row| row.edge_id == edge)
            .expect("edge endpoint kind row");
        assert_eq!(endpoint_kind.source_kind, EntityKind::Abstraction);
        assert_eq!(
            endpoint_kind.target_kind, None,
            "redacted target kind must not be exposed through ListChangeEventsReadResponse"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn query_high_water_includes_readable_non_world_source_edge_events()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        let gp = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let private = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let p = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let a_source = seed_memory(&pg, &gp, EntityKind::Abstraction, "A source").await?;
        let f_private = seed_memory(&pg, &private, EntityKind::Fact, "private").await?;

        let edge = seed_memory_edge(
            &pg,
            &gp,
            (EntityKind::Abstraction, a_source),
            (EntityKind::Fact, f_private),
            CORE_DERIVED_FROM_RELATION,
            RelationClass::Provenance,
        )
        .await?;
        let visible_seq = insert_edge_append_event(&pg, &gp, edge, a_source, f_private).await?;

        let p_read = vec![p, gp];
        let query = pg
            .query_memories(
                &QueryRequest {
                    owner: p,
                    read_owners: p_read,
                    entity_kind: None,
                    schema_id: None,
                    supersession: SupersessionStatus::HeadsOnly,
                    tombstones: TombstoneFilter::PresentOnly,
                    goal_state: None,
                    limit: 10,
                    page: proxima_core::verbs::query::QueryPage::default(),
                    include_payloads: false,
                    memory_ids: Vec::new(),
                    goal_ids: Vec::new(),
                    edge_ids: Vec::new(),
                    stateful_heads: Vec::new(),
                },
                &[],
            )
            .await?;

        assert_eq!(
            query.seq_high_water,
            Some(visible_seq),
            "query high-water includes visible source-owned edge event seq"
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
    owner: &OwnerRef,
    edge_id: EdgeId,
    source_memory_id: MemoryId,
    target_memory_id: MemoryId,
) -> Result<Uuid, sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    let seq = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.change_event \
            (seq, owner_kind, owner_id, kind, \
             edge_id, edge_relation, \
             edge_source_memory_id, edge_source_goal_id, edge_source_fact_entity_id, \
             edge_target_memory_id, edge_target_goal_id, edge_target_fact_entity_id) \
         VALUES ($1, $2, $3, 'EdgeAppend', $4, $5, $6, NULL, NULL, $7, NULL, NULL)",
    )
    .bind(seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(edge_id.into_inner())
    .bind(CORE_DERIVED_FROM_RELATION)
    .bind(source_memory_id.into_inner())
    .bind(target_memory_id.into_inner())
    .execute(pg.pool_for_tests())
    .await
    .map(|_| seq)
}
