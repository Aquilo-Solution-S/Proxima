//! End-to-end outbox publisher test against a transient PG database.

mod common;

use common::{create_db, db_url, drop_db};
use std::sync::Arc;
use std::time::Duration;

use proxima_core::engine::Engine;
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::verbs::goal_write::{GoalAuthorship, GoalDraft, GoalState};
use proxima_core::verbs::query::MemoryStore;
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    ChangeEventKind, EntityKind, EntityRef, OrgId, Owner, Principal, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, UserId,
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
            cbor_encoder: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/cited_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitedObject,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            cbor_encoder: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/citation_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::CitationMapping,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            cbor_encoder: None,
        },
        SchemaInfo {
            schema_id: SchemaId::new("test/goal_blob".into()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Goal,
            filter_keys: vec![],
            sidecar_table: None,
            natural_key_columns: vec![],
            tombstone: None,
            cbor_encoder: None,
        },
    ]
}

fn fresh_event_draft(owner: Owner) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner,
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload: b"hello world".to_vec(),
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("test/cited_blob".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: [42u8; 32],
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("test/citation_blob".into()),
            schema_version: SchemaVersion::new(1),
        },
    }
}

fn fresh_goal_draft(owner: &Owner, request_id: String) -> GoalDraft {
    GoalDraft {
        owner: owner.clone(),
        schema_id: SchemaId::new("test/goal_blob".into()),
        schema_version: SchemaVersion::new(1),
        title: "Test goal".to_string(),
        text: "Test goal text".to_string(),
        payload: Vec::new(),
        state: GoalState::Active,
        parent_goal_ids: vec![],
        supersedes_goal_id: None,
        authorship: GoalAuthorship::User,
        request_id,
    }
}

#[tokio::test]
async fn outbox_publishes_entity_append_for_fact() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        pg.start_outbox().await?;

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry, MemoryStore::new()).with_storage(storage);

        let mut rx = pg.changes();

        // Ingest an event — should produce a ChangeEvent.
        let draft = fresh_event_draft(owner.clone());
        let outcome = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft.clone(),
            )
            .await?;
        assert!(!outcome.idempotent_replay);

        // Wait for the ChangeEvent to arrive.
        let ce = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await??;

        // Verify the ChangeEvent.
        assert_eq!(ce.seq, outcome.change_event_seq);
        assert_eq!(ce.owner.principal, owner.principal);
        assert_eq!(ce.owner.org_id, owner.org_id);

        match &ce.kind {
            ChangeEventKind::EntityAppend {
                entity_kind,
                entity,
                schema_id,
                schema_version,
                supersedes,
            } => {
                assert_eq!(*entity_kind, EntityKind::Fact);
                assert_eq!(*entity, EntityRef::Memory(outcome.memory_id));
                assert_eq!(*schema_id, draft.schema_id);
                assert_eq!(*schema_version, draft.schema_version);
                assert!(supersedes.is_none());
            }
            ChangeEventKind::EdgeAppend { .. } => panic!("expected EntityAppend"),
        }

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("outbox_publishes_entity_append_for_fact test failed");
}

#[tokio::test]
async fn outbox_publishes_entity_append_for_goal() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        pg.start_outbox().await?;

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry, MemoryStore::new()).with_storage(storage);

        let mut rx = pg.changes();

        // Write a goal — should produce a ChangeEvent.
        let draft = fresh_goal_draft(&owner, "req-1".to_string());
        let outcome = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                draft.clone(),
            )
            .await?;
        assert!(!outcome.idempotent_replay);

        // Wait for the ChangeEvent to arrive.
        let ce = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await??;

        // Verify the ChangeEvent.
        assert_eq!(ce.seq, outcome.change_event_seq);
        assert_eq!(ce.owner.principal, owner.principal);
        assert_eq!(ce.owner.org_id, owner.org_id);

        match &ce.kind {
            ChangeEventKind::EntityAppend {
                entity_kind,
                entity,
                schema_id,
                schema_version,
                supersedes,
            } => {
                assert_eq!(*entity_kind, EntityKind::Goal);
                assert_eq!(*entity, EntityRef::Goal(outcome.goal_id));
                assert_eq!(*schema_id, draft.schema_id);
                assert_eq!(*schema_version, draft.schema_version);
                assert!(supersedes.is_none());
            }
            ChangeEventKind::EdgeAppend { .. } => panic!("expected EntityAppend"),
        }

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("outbox_publishes_entity_append_for_goal test failed");
}

#[tokio::test]
async fn outbox_publishes_fact_then_goal() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        pg.start_outbox().await?;

        let storage: Arc<dyn Storage> = Arc::new(pg.clone());

        let user = UserId::new(Uuid::now_v7());
        let owner = Owner {
            principal: Principal::User(user),
            org_id: OrgId::new(Uuid::now_v7()),
        };

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry, MemoryStore::new()).with_storage(storage);

        let mut rx = pg.changes();

        // Ingest an event.
        let event_draft = fresh_event_draft(owner.clone());
        let event_outcome = engine
            .event_ingest(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                event_draft.clone(),
            )
            .await?;

        // Receive first ChangeEvent (Fact).
        let fact_ce = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await??;
        assert_eq!(fact_ce.seq, event_outcome.change_event_seq);
        match &fact_ce.kind {
            ChangeEventKind::EntityAppend { entity_kind, .. } => {
                assert_eq!(*entity_kind, EntityKind::Fact);
            }
            ChangeEventKind::EdgeAppend { .. } => panic!("expected EntityAppend"),
        }

        // Write a goal.
        let goal_draft = fresh_goal_draft(&owner, "req-1".to_string());
        let goal_outcome = engine
            .write_goal(
                &proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::System),
                goal_draft.clone(),
            )
            .await?;

        // Receive second ChangeEvent (Goal).
        let goal_ce = tokio::time::timeout(Duration::from_secs(10), rx.recv()).await??;
        assert_eq!(goal_ce.seq, goal_outcome.change_event_seq);
        match &goal_ce.kind {
            ChangeEventKind::EntityAppend { entity_kind, .. } => {
                assert_eq!(*entity_kind, EntityKind::Goal);
            }
            ChangeEventKind::EdgeAppend { .. } => panic!("expected EntityAppend"),
        }

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("outbox_publishes_fact_then_goal test failed");
}
