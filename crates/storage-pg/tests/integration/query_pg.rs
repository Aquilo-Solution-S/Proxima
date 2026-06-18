//! End-to-end `Query` against a transient PG database.

use crate::common::{create_db, db_url, drop_db};
use std::sync::Arc;

use proxima_core::engine::Engine;
use proxima_core::personality::ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::verbs::goal_write::{GoalAuthorshipKind, GoalState};
use proxima_core::verbs::query::{
    EntityKind, PersonalityRootFilter, QueryRequest, SupersessionStatus,
};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    OrgId, Owner, OwnerPrincipalKind, Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId,
    UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use proxima_core::PersonalityStatus;

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
    let owner_principal_id = match owner.principal {
        Principal::User(u) => u.into_inner(),
        Principal::Group(g) => g.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, schema_id, schema_version, owner_principal_kind,
             owner_principal_id, owner_org_id, title, text, payload, state,
             authorship_kind, request_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(Uuid::now_v7())
    .bind(schema_id)
    .bind(schema_version)
    .bind(OwnerPrincipalKind::of(&owner.principal))
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(title)
    .bind(text)
    .bind(payload)
    .bind(GoalState::Active)
    .bind(GoalAuthorshipKind::User)
    .bind(format!("seed-{}", Uuid::now_v7()))
    .execute(pg.pool())
    .await
    .map(|_| ())
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

fn schemas_for_personality_root_test() -> Vec<SchemaInfo> {
    let mut schemas = schemas_for_test();
    schemas.push(SchemaInfo::opaque(
        SchemaId::new("proxima-code/engineer-self-v1".into()),
        SchemaVersion::new(1),
        PayloadKind::Perspective,
    ));
    schemas.push(SchemaInfo::opaque(
        SchemaId::new("test/development-perspective-v1".into()),
        SchemaVersion::new(1),
        PayloadKind::Perspective,
    ));
    schemas
}

fn fresh_draft(owner: Owner) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner.principal,
        org_id: Some(owner.org_id),
        author_personality_instance_id: None,
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload: b"hello world".to_vec(),
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

async fn insert_test_edge(
    pg: &PgStorage,
    owner: &Owner,
    source: Uuid,
    target: Uuid,
    created_offset_seconds: i64,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let edge_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.edges
           (edge_id, relation, relation_class,
            source_kind, source_memory_id, source_goal_id,
            target_kind, target_memory_id, target_goal_id,
            authorship_kind, authorship_owner_memory_id,
            owner_principal_kind, owner_principal_id, owner_org_id, created_at)
         VALUES
           ($1, 'test/structural', 'Structural',
            'Fact', $2, NULL,
            'Fact', $3, NULL,
            'EventSource', NULL,
            $4, $5, $6, now() + ($7 * interval '1 second'))",
    )
    .bind(edge_id)
    .bind(source)
    .bind(target)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(created_offset_seconds)
    .execute(pg.pool())
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
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    let edge_ids: Vec<Uuid> = (0..count).map(|_| Uuid::now_v7()).collect();
    sqlx::query(
        "INSERT INTO proxima_core.edges
           (edge_id, relation, relation_class,
            source_kind, source_memory_id, source_goal_id,
            target_kind, target_memory_id, target_goal_id,
            authorship_kind, authorship_owner_memory_id,
            owner_principal_kind, owner_principal_id, owner_org_id, created_at)
         SELECT ids.edge_id, 'test/structural', 'Structural',
                'Fact', $1, NULL,
                'Fact', $2, NULL,
                'EventSource', NULL,
                $3, $4, $5, now() + (ids.ord * interval '1 microsecond')
         FROM unnest($6::uuid[]) WITH ORDINALITY AS ids(edge_id, ord)",
    )
    .bind(source)
    .bind(target)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(edge_ids)
    .execute(pg.pool())
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
    .execute(pg.pool())
    .await?;
    Ok(())
}

async fn insert_perspective_memory(
    pg: &PgStorage,
    owner: &Owner,
    schema_id: &str,
    text: &str,
    personality_status: Option<PersonalityStatus>,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let instance_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, $5, 1, 'Perspective', $6, 'Wake',
                 'substrate', 'test-v1', $7, 0)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(schema_id)
    .bind(text)
    .bind(instance_id)
    .execute(pg.pool())
    .await?;

    if let Some(status) = personality_status {
        let tombstoned_at = if matches!(status, PersonalityStatus::Tombstoned) {
            Some(time::OffsetDateTime::now_utc())
        } else {
            None
        };
        sqlx::query(
            "INSERT INTO proxima_core.personality
                (owner_principal_kind, owner_principal_id, owner_org_id,
                 personality_instance_id, current_root_perspective_memory_id,
                 status, tombstoned_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(owner_kind)
        .bind(owner_principal_id)
        .bind(owner.org_id.into_inner())
        .bind(instance_id)
        .bind(memory_id)
        .bind(status)
        .bind(tombstoned_at)
        .execute(pg.pool())
        .await?;
    }

    Ok(memory_id)
}

#[tokio::test]
async fn query_returns_stored_schema_version() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage(storage);

        let mut draft = fresh_draft(owner.clone());
        draft.schema_id = SchemaId::new("test/fact_blob_v2".into());
        draft.schema_version = SchemaVersion::new(2);
        engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft,
            )
            .await?;

        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &QueryRequest::for_principal(owner.principal.clone()),
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
async fn query_active_only_filters_inactive_personality_roots() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_personality_root_test());
        let engine = Engine::new(registry).with_storage(storage);

        let active_root = insert_perspective_memory(
            &pg,
            &owner,
            ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID,
            "active root",
            Some(PersonalityStatus::Active),
        )
        .await?;
        let tombstoned_root = insert_perspective_memory(
            &pg,
            &owner,
            "proxima-code/engineer-self-v1",
            "tombstoned root",
            Some(PersonalityStatus::Tombstoned),
        )
        .await?;
        let orphan_root = insert_perspective_memory(
            &pg,
            &owner,
            "proxima-code/engineer-self-v1",
            "orphan root",
            None,
        )
        .await?;
        let normal_perspective = insert_perspective_memory(
            &pg,
            &owner,
            "test/development-perspective-v1",
            "normal perspective",
            None,
        )
        .await?;

        let mut include_inactive = QueryRequest::for_principal(owner.principal.clone());
        include_inactive.personality_roots = PersonalityRootFilter::IncludeInactive;
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &include_inactive,
            )
            .await?;
        let all_ids = resp
            .memories
            .iter()
            .map(|row| row.id.into_inner())
            .collect::<std::collections::HashSet<_>>();
        assert!(all_ids.contains(&active_root));
        assert!(all_ids.contains(&tombstoned_root));
        assert!(all_ids.contains(&orphan_root));
        assert!(all_ids.contains(&normal_perspective));

        let mut active_only = QueryRequest::for_principal(owner.principal.clone());
        active_only.personality_roots = PersonalityRootFilter::ActiveOnly;
        let resp = engine
            .query(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                &active_only,
            )
            .await?;
        let active_ids = resp
            .memories
            .iter()
            .map(|row| row.id.into_inner())
            .collect::<std::collections::HashSet<_>>();
        assert!(active_ids.contains(&active_root));
        assert!(active_ids.contains(&normal_perspective));
        assert!(!active_ids.contains(&tombstoned_root));
        assert!(!active_ids.contains(&orphan_root));

        Ok(())
    }
    .await;

    let drop_result = drop_db(&db_name).await;
    if let Err(e) = result {
        panic!("{e}");
    }
    drop_result.expect("drop test db");
}

#[tokio::test]
async fn query_returns_fact_rows() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage(storage);

        // Ingest two distinct Facts.
        let draft1 = fresh_draft(owner.clone());
        let draft2 = {
            let mut d = fresh_draft(owner.clone());
            d.payload = b"another fact".to_vec();
            d.source_batch_id = SourceBatchId::new(Uuid::now_v7());
            d
        };

        let outcome1 = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft1.clone(),
            )
            .await?;
        let outcome2 = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft2.clone(),
            )
            .await?;

        // Query for all memories for this owner.
        let req = QueryRequest::for_principal(owner.principal.clone());
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
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage(storage);

        let first = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                fresh_draft(owner.clone()),
            )
            .await?
            .memory_id;
        let second = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                {
                    let mut draft = fresh_draft(owner.clone());
                    draft.payload = b"second".to_vec();
                    draft.source_batch_id = SourceBatchId::new(Uuid::now_v7());
                    draft
                },
            )
            .await?
            .memory_id;

        let e1 = insert_test_edge(&pg, &owner, first.into_inner(), second.into_inner(), 1).await?;
        let e2 = insert_test_edge(&pg, &owner, first.into_inner(), second.into_inner(), 2).await?;
        let e3 = insert_test_edge(&pg, &owner, first.into_inner(), second.into_inner(), 3).await?;

        let mut req = QueryRequest::for_principal(owner.principal.clone());
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
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage(storage);

        let outside = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                fresh_draft(owner.clone()),
            )
            .await?
            .memory_id;
        let inside_a = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                {
                    let mut draft = fresh_draft(owner.clone());
                    draft.payload = b"inside-a".to_vec();
                    draft.source_batch_id = SourceBatchId::new(Uuid::now_v7());
                    draft
                },
            )
            .await?
            .memory_id;
        let inside_b = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                {
                    let mut draft = fresh_draft(owner.clone());
                    draft.payload = b"inside-b".to_vec();
                    draft.source_batch_id = SourceBatchId::new(Uuid::now_v7());
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

        let mut req = QueryRequest::for_principal(owner.principal.clone());
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
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage(storage);

        let a = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                fresh_draft(owner.clone()),
            )
            .await?
            .memory_id;
        let b = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                {
                    let mut draft = fresh_draft(owner.clone());
                    draft.payload = b"target".to_vec();
                    draft.source_batch_id = SourceBatchId::new(Uuid::now_v7());
                    draft
                },
            )
            .await?
            .memory_id;
        let edge_id = insert_test_edge(&pg, &owner, a.into_inner(), b.into_inner(), 1).await?;

        let mut req = QueryRequest::for_principal(owner.principal.clone());
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
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage(storage);

        let a = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                fresh_draft(owner.clone()),
            )
            .await?
            .memory_id;
        let b = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                {
                    let mut draft = fresh_draft(owner.clone());
                    draft.payload = b"second".to_vec();
                    draft.source_batch_id = SourceBatchId::new(Uuid::now_v7());
                    draft
                },
            )
            .await?
            .memory_id;

        let total = proxima_storage_pg::query::MAX_SNAPSHOT_EDGES + 1;
        insert_n_test_edges_bulk(&pg, &owner, a.into_inner(), b.into_inner(), total).await?;

        let mut req = QueryRequest::for_principal(owner.principal.clone());
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
async fn query_owner_scope_ignores_org_id() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let stored_owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let requested_owner = Owner {
            principal: stored_owner.principal.clone(),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage(storage);

        let draft = fresh_draft(stored_owner.clone());
        let outcome = engine
            .event_ingest(
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
                &QueryRequest::for_principal(requested_owner.principal.clone()),
            )
            .await?;

        assert_eq!(resp.memories.len(), 1);
        assert_eq!(resp.memories[0].owner, stored_owner);
        assert_eq!(resp.seq_high_water, Some(outcome.change_event_seq));

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("query_owner_scope_ignores_org_id test failed");
}

#[tokio::test]
async fn query_filter_abstraction_returns_empty() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage(storage);

        // Ingest a Fact.
        let draft = fresh_draft(owner.clone());
        engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft,
            )
            .await?;

        // Query with entity_kind = Abstraction filter.
        let req = QueryRequest {
            principal: owner.principal.clone(),
            entity_kind: Some(EntityKind::Abstraction),
            schema_id: None,
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            personality_roots: PersonalityRootFilter::IncludeInactive,
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
async fn query_goals_filter_by_schema_id() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage(storage);
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
            principal: owner.principal.clone(),
            entity_kind: Some(EntityKind::Goal),
            schema_id: Some(SchemaId::new("test/fact_blob".into())),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            personality_roots: PersonalityRootFilter::IncludeInactive,
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
            principal: owner.principal.clone(),
            entity_kind: Some(EntityKind::Goal),
            schema_id: Some(SchemaId::new("test/goal_blob".into())),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            personality_roots: PersonalityRootFilter::IncludeInactive,
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
            principal: owner.principal.clone(),
            entity_kind: None,
            schema_id: Some(SchemaId::new("test/never_registered".into())),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            personality_roots: PersonalityRootFilter::IncludeInactive,
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
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage(storage);

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
                &QueryRequest::for_principal(owner.principal.clone()),
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
        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage(storage);

        // Ingest a Fact.
        let draft = fresh_draft(owner.clone());
        engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft,
            )
            .await?;

        // Query with non-existent schema_id filter.
        let req = QueryRequest {
            principal: owner.principal.clone(),
            entity_kind: None,
            schema_id: Some(SchemaId::new("test/non_existent".into())),
            supersession: SupersessionStatus::HeadsOnly,
            tombstones: proxima_core::verbs::query::TombstoneFilter::PresentOnly,
            personality_roots: PersonalityRootFilter::IncludeInactive,
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
