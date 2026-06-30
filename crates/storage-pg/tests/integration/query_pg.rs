//! End-to-end `Query` against a transient PG database.

use crate::common::{create_db, db_url, drop_db};
use std::sync::Arc;

use proxima_core::engine::Engine;
use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::verbs::goal_write::{GoalAuthorshipKind, GoalState};
use proxima_core::verbs::query::{EntityKind, QueryRequest, SupersessionStatus};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{Owner, OwnerRef, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

/// Seed an Active, User-authored goal row directly (the query tests only
/// need a queryable goal row; the full create-goal atom would require a
/// self-perspective + lifecycle machinery these tests don't exercise).
async fn seed_goal(
    pg: &PgStorage,
    owner: &Owner,
    schema_id: &str,
    schema_version: i32,
    title: &str,
    text: &str,
    payload: &[u8],
) -> Result<(), sqlx::Error> {
    let goal_id = Uuid::now_v7();
    let (owner_kind, owner_id) = owner.columns();
    let request_id = format!("seed-{}", Uuid::now_v7());
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, owner_kind, owner_id, schema_id, schema_version, title, text, payload, state,
             authorship_kind, request_id, idempotency_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                 md5($2::text || ':' || $3::text || ':' || $11))",
    )
    .bind(goal_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(schema_id)
    .bind(schema_version)
    .bind(title)
    .bind(text)
    .bind(payload)
    .bind(GoalState::Active)
    .bind(GoalAuthorshipKind::User)
    .bind(request_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

async fn insert_abstraction_memory(
    pg: &PgStorage,
    owner: &Owner,
    text: &str,
    supersedes: Option<Uuid>,
) -> Result<Uuid, sqlx::Error> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version, supersedes)
         VALUES ($1, $2, $3, 'test/head-abstraction-v1', 1, 'Abstraction', $4,
                 'AtoA', '00000000-0000-0000-0000-000000000301'::uuid,
                 '00000000-0000-0000-0000-000000000302'::uuid, NULL,
                 'test-model', 'query-heads-v1', $5)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .bind(supersedes)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(memory_id)
}

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
            schema_id: SchemaId::new("test/fact_blob_v2".into()),
            schema_version: SchemaVersion::new(2),
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
        SchemaInfo {
            schema_id: SchemaId::new("test/goal_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Goal,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            has_typed_ingress: false,
            cited_object_schema: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/goal_blob_v2".into()),
            schema_version: SchemaVersion::new(2),
            kind: PayloadKind::Goal,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            has_typed_ingress: false,
            cited_object_schema: None,
        },
    ]
}

fn fresh_draft(_owner: Owner) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload: b"hello world".to_vec(),
        rendered_text: None,
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

async fn insert_test_edge(
    pg: &PgStorage,
    owner: &Owner,
    source: Uuid,
    target: Uuid,
    created_offset_seconds: i64,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let edge_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
           (edge_id, owner_kind, owner_id, relation, relation_class,
            source_kind, source_memory_id, source_goal_id,
            target_kind, target_memory_id, target_goal_id,
            authorship_kind, authorship_owner_memory_id, created_at)
         VALUES
           ($1, $2, $3, 'test/structural', 'Structural',
            'Fact', $4, NULL,
            'Fact', $5, NULL,
            'EventSource', NULL,
            now() + ($6 * interval '1 second'))",
    )
    .bind(edge_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(source)
    .bind(target)
    .bind(created_offset_seconds)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(edge_id)
}

async fn insert_n_test_edges_bulk(
    pg: &PgStorage,
    owner: &Owner,
    source: Uuid,
    target: Uuid,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let edge_ids: Vec<Uuid> = (0..count).map(|_| Uuid::now_v7()).collect();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
           (edge_id, owner_kind, owner_id, relation, relation_class,
            source_kind, source_memory_id, source_goal_id,
            target_kind, target_memory_id, target_goal_id,
            authorship_kind, authorship_owner_memory_id, created_at)
         SELECT ids.edge_id, $3, $4, 'test/structural', 'Structural',
                'Fact', $1, NULL,
                'Fact', $2, NULL,
                'EventSource', NULL,
                now() + (ids.ord * interval '1 microsecond')
         FROM unnest($5::uuid[]) WITH ORDINALITY AS ids(edge_id, ord)",
    )
    .bind(source)
    .bind(target)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(edge_ids)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

async fn set_memory_created_offset(
    pg: &PgStorage,
    memory_id: Uuid,
    created_offset_seconds: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "UPDATE proxima_core.memories
         SET created_at = now() + ($2 * interval '1 second')
         WHERE memory_id = $1",
    )
    .bind(memory_id)
    .bind(created_offset_seconds)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(())
}

#[tokio::test]
async fn query_returns_stored_schema_version() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        let mut draft = fresh_draft(owner);
        draft.schema_id = SchemaId::new("test/fact_blob_v2".into());
        draft.schema_version = SchemaVersion::new(2);
        engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft,
            )
            .await?;

        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &QueryRequest::for_principal(owner),
            )
            .await?;

        assert_eq!(resp.memories.len(), 1);
        assert_eq!(
            resp.memories[0].schema_id,
            SchemaId::new("test/fact_blob_v2".into())
        );
        assert_eq!(resp.memories[0].schema_version, SchemaVersion::new(2));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_returns_stored_schema_version test failed");
}

#[tokio::test]
async fn query_returns_fact_rows() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        // Ingest two distinct Facts.
        let draft1 = fresh_draft(owner);
        let draft2 = {
            let mut d = fresh_draft(owner);
            d.payload = b"another fact".to_vec();
            d.receipt.as_mut().expect("receipt").source_batch_id =
                SourceBatchId::new(Uuid::now_v7());
            d
        };

        let outcome1 = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft1.clone(),
            )
            .await?;
        let outcome2 = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft2.clone(),
            )
            .await?;

        // Query for all memories for this owner.
        let req = QueryRequest::for_principal(owner);
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &req,
            )
            .await?;

        assert_eq!(resp.memories.len(), 2);
        for m in &resp.memories {
            assert_eq!(m.kind, EntityKind::Fact);
            assert_eq!(m.schema_id, SchemaId::new("test/fact_blob".into()));
            assert_eq!(m.owner, owner);
        }

        // seq_high_water should be Some and equal to the greater of the two seqs.
        assert!(resp.seq_high_water.is_some());
        let expected_max = std::cmp::max(outcome1.change_event_seq, outcome2.change_event_seq);
        assert_eq!(resp.seq_high_water, Some(expected_max));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_returns_fact_rows test failed");
}

#[tokio::test]
async fn query_returns_all_edges_between_returned_nodes_even_when_edge_count_exceeds_limit() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        let first = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                fresh_draft(owner),
            )
            .await?
            .memory_id;
        let second = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                {
                    let mut draft = fresh_draft(owner);
                    draft.payload = b"second".to_vec();
                    draft.receipt.as_mut().expect("receipt").source_batch_id =
                        SourceBatchId::new(Uuid::now_v7());
                    draft
                },
            )
            .await?
            .memory_id;

        let e1 = insert_test_edge(&pg, &owner, first.into_inner(), second.into_inner(), 1).await?;
        let e2 = insert_test_edge(&pg, &owner, first.into_inner(), second.into_inner(), 2).await?;
        let e3 = insert_test_edge(&pg, &owner, first.into_inner(), second.into_inner(), 3).await?;

        let mut req = QueryRequest::for_principal(owner);
        req.limit = 2;
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &req,
            )
            .await?;

        assert_eq!(resp.memories.len(), 2);
        let edge_ids = resp
            .edges
            .iter()
            .map(|edge| edge.id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(edge_ids, std::collections::BTreeSet::from([e1, e2, e3]));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect(
        "query_returns_all_edges_between_returned_nodes_even_when_edge_count_exceeds_limit failed",
    );
}

#[tokio::test]
async fn query_excludes_edges_with_endpoint_outside_returned_node_window() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        let outside = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                fresh_draft(owner),
            )
            .await?
            .memory_id;
        let inside_a = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                {
                    let mut draft = fresh_draft(owner);
                    draft.payload = b"inside-a".to_vec();
                    draft.receipt.as_mut().expect("receipt").source_batch_id =
                        SourceBatchId::new(Uuid::now_v7());
                    draft
                },
            )
            .await?
            .memory_id;
        let inside_b = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                {
                    let mut draft = fresh_draft(owner);
                    draft.payload = b"inside-b".to_vec();
                    draft.receipt.as_mut().expect("receipt").source_batch_id =
                        SourceBatchId::new(Uuid::now_v7());
                    draft
                },
            )
            .await?
            .memory_id;

        set_memory_created_offset(&pg, outside.into_inner(), 1).await?;
        set_memory_created_offset(&pg, inside_a.into_inner(), 2).await?;
        set_memory_created_offset(&pg, inside_b.into_inner(), 3).await?;

        let visible_edge =
            insert_test_edge(&pg, &owner, inside_a.into_inner(), inside_b.into_inner(), 1).await?;
        let hidden_edge =
            insert_test_edge(&pg, &owner, outside.into_inner(), inside_b.into_inner(), 2).await?;

        let mut req = QueryRequest::for_principal(owner);
        req.limit = 2;
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &req,
            )
            .await?;

        assert_eq!(resp.memories.len(), 2);
        let edge_ids = resp
            .edges
            .iter()
            .map(|edge| edge.id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(edge_ids, std::collections::BTreeSet::from([visible_edge]));
        assert!(!edge_ids.contains(&hidden_edge));
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_excludes_edges_with_endpoint_outside_returned_node_window failed");
}

#[tokio::test]
async fn query_edge_id_hydration_returns_requested_edge_without_visible_nodes() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        let a = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                fresh_draft(owner),
            )
            .await?
            .memory_id;
        let b = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                {
                    let mut draft = fresh_draft(owner);
                    draft.payload = b"target".to_vec();
                    draft.receipt.as_mut().expect("receipt").source_batch_id =
                        SourceBatchId::new(Uuid::now_v7());
                    draft
                },
            )
            .await?
            .memory_id;
        let edge_id = insert_test_edge(&pg, &owner, a.into_inner(), b.into_inner(), 1).await?;

        let mut req = QueryRequest::for_principal(owner);
        req.limit = 1;
        req.edge_ids = vec![edge_id];
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &req,
            )
            .await?;

        assert!(resp.memories.is_empty());
        assert_eq!(resp.edges.len(), 1);
        assert_eq!(resp.edges[0].id, edge_id);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_edge_id_hydration_returns_requested_edge_without_visible_nodes failed");
}

#[tokio::test]
async fn query_caps_snapshot_edges_at_max_snapshot_edges() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        let a = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                fresh_draft(owner),
            )
            .await?
            .memory_id;
        let b = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                {
                    let mut draft = fresh_draft(owner);
                    draft.payload = b"second".to_vec();
                    draft.receipt.as_mut().expect("receipt").source_batch_id =
                        SourceBatchId::new(Uuid::now_v7());
                    draft
                },
            )
            .await?
            .memory_id;

        let total = proxima_storage_pg::query::MAX_SNAPSHOT_EDGES + 1;
        insert_n_test_edges_bulk(&pg, &owner, a.into_inner(), b.into_inner(), total).await?;

        let mut req = QueryRequest::for_principal(owner);
        req.limit = 2;
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &req,
            )
            .await?;

        assert_eq!(resp.memories.len(), 2);
        assert_eq!(
            resp.edges.len(),
            proxima_storage_pg::query::MAX_SNAPSHOT_EDGES
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_caps_snapshot_edges_at_max_snapshot_edges failed");
}

#[tokio::test]
async fn query_owner_scope_is_principal() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let stored_owner = OwnerRef::Personal(user);
        let requested_owner = stored_owner;

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        let draft = fresh_draft(stored_owner);
        let outcome = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(
                    &stored_owner,
                    proxima_core::AuthPath::System,
                ),
                draft.clone(),
            )
            .await?;

        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(
                    &stored_owner,
                    proxima_core::AuthPath::System,
                ),
                &QueryRequest::for_principal(requested_owner),
            )
            .await?;

        assert_eq!(resp.memories.len(), 1);
        assert_eq!(resp.memories[0].owner, stored_owner);
        assert_eq!(resp.seq_high_water, Some(outcome.change_event_seq));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_owner_scope_is_principal test failed");
}

#[tokio::test]
async fn query_filter_abstraction_returns_empty() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        // Ingest a Fact.
        let draft = fresh_draft(owner);
        engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft,
            )
            .await?;

        // Query with entity_kind = Abstraction filter.
        let req = QueryRequest {
            principal: owner,
            read_owners: vec![owner],
            entity_kind: Some(EntityKind::Abstraction),
            schema_id: None,
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            limit: 100,
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        };
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &req,
            )
            .await?;

        assert!(resp.memories.is_empty());

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_filter_abstraction_returns_empty test failed");
}

#[tokio::test]
async fn query_heads_only_ignores_cross_owner_supersedes_successor() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let victim = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let attacker = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let foreign_shadowed =
            insert_abstraction_memory(&pg, &victim, "victim head with foreign successor", None)
                .await?;
        let foreign_successor = insert_abstraction_memory(
            &pg,
            &attacker,
            "attacker corrupt successor",
            Some(foreign_shadowed),
        )
        .await?;
        let same_owner_shadowed =
            insert_abstraction_memory(&pg, &victim, "victim superseded head", None).await?;
        let same_owner_successor = insert_abstraction_memory(
            &pg,
            &victim,
            "victim same-owner successor",
            Some(same_owner_shadowed),
        )
        .await?;

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);
        let req = QueryRequest {
            principal: victim,
            read_owners: vec![victim],
            entity_kind: Some(EntityKind::Abstraction),
            schema_id: Some(SchemaId::new("test/head-abstraction-v1".into())),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            limit: 100,
            include_payloads: false,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        };
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&victim, proxima_core::AuthPath::System),
                &req,
            )
            .await?;
        let ids = resp
            .memories
            .iter()
            .map(|row| row.id.into_inner())
            .collect::<Vec<_>>();

        assert!(
            ids.contains(&foreign_shadowed),
            "foreign successor must not suppress victim head: {ids:#?}"
        );
        assert!(
            !ids.contains(&foreign_successor),
            "attacker successor must remain unreadable: {ids:#?}"
        );
        assert!(
            !ids.contains(&same_owner_shadowed),
            "same-owner successor must suppress prior head: {ids:#?}"
        );
        assert!(
            ids.contains(&same_owner_successor),
            "same-owner successor remains the victim head: {ids:#?}"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_heads_only_ignores_cross_owner_supersedes_successor test failed");
}

#[tokio::test]
async fn query_goals_filter_by_schema_id() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);
        let authz =
            proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System);

        // Seed a goal under "test/goal_blob" v1.
        seed_goal(
            &pg,
            &owner,
            "test/goal_blob",
            1,
            "Test goal",
            "v1 goal",
            br#"{"v":1}"#,
        )
        .await?;

        // Filtering by a Fact schema_id must return zero goals.
        let req_fact_filter = QueryRequest {
            principal: owner,
            read_owners: vec![owner],
            entity_kind: Some(EntityKind::Goal),
            schema_id: Some(SchemaId::new("test/fact_blob".into())),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            limit: 100,
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        };
        let resp = engine.query(&authz, &req_fact_filter).await?;
        assert!(
            resp.goals.is_empty(),
            "expected zero goals when filtering by Fact schema, got {}",
            resp.goals.len()
        );

        // Filtering by the matching goal schema_id returns the goal.
        let req_goal_filter = QueryRequest {
            principal: owner,
            read_owners: vec![owner],
            entity_kind: Some(EntityKind::Goal),
            schema_id: Some(SchemaId::new("test/goal_blob".into())),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            limit: 100,
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        };
        let resp = engine.query(&authz, &req_goal_filter).await?;
        assert_eq!(resp.goals.len(), 1);

        // Filtering by a non-existent schema_id returns zero goals.
        let req_unknown = QueryRequest {
            principal: owner,
            read_owners: vec![owner],
            entity_kind: None,
            schema_id: Some(SchemaId::new("test/never_registered".into())),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            limit: 100,
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        };
        let resp = engine.query(&authz, &req_unknown).await?;
        assert!(resp.goals.is_empty());

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_goals_filter_by_schema_id test failed");
}

#[tokio::test]
async fn query_returns_stored_goal_schema_version() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        // Seed a goal under schema_version=2.
        seed_goal(
            &pg,
            &owner,
            "test/goal_blob_v2",
            2,
            "Test goal",
            "v2 goal",
            br#"{"v":2}"#,
        )
        .await?;

        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &QueryRequest::for_principal(owner),
            )
            .await?;
        assert_eq!(resp.goals.len(), 1);
        assert_eq!(
            resp.goals[0].schema_id,
            SchemaId::new("test/goal_blob_v2".into())
        );
        assert_eq!(resp.goals[0].schema_version, SchemaVersion::new(2));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_returns_stored_goal_schema_version test failed");
}

#[tokio::test]
async fn query_filter_nonexistent_schema_returns_empty() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        // Ingest a Fact.
        let draft = fresh_draft(owner);
        engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft,
            )
            .await?;

        // Query with non-existent schema_id filter.
        let req = QueryRequest {
            principal: owner,
            read_owners: vec![owner],
            entity_kind: None,
            schema_id: Some(SchemaId::new("test/non_existent".into())),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            limit: 100,
            include_payloads: true,
            memory_ids: Vec::new(),
            goal_ids: Vec::new(),
            edge_ids: Vec::new(),
            stateful_heads: Vec::new(),
        };
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &req,
            )
            .await?;

        assert!(resp.memories.is_empty());

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_filter_nonexistent_schema_returns_empty test failed");
}
