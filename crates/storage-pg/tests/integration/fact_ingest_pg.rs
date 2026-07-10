//! End-to-end `FactIngest` against a transient PG database.

use crate::common::{create_db, db_url, drop_db, owner_write_permit};
use std::sync::Arc;

use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::storage_ports::*;
use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    GroupId, Owner, OwnerRef, Relation, Role, SchemaId, SchemaVersion, SourceBatchId, SourceId,
    UserId,
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

fn fresh_draft(_owner: Owner) -> FactWriteCommand {
    // Receipt identity is blake3(source_id, owner, payload) — distinct
    // receipt-backed Facts need distinct payloads, not just distinct batch ids.
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload: format!("hello world {}", Uuid::now_v7()).into_bytes(),
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

#[tokio::test]
async fn fact_ingest_writes_fact_and_change_event() {
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

        let draft = fresh_draft(owner);

        let outcome = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                draft.clone(),
            )
            .await?;
        assert!(!outcome.idempotent_replay);

        let replay = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                draft.clone(),
            )
            .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, outcome.memory_id);

        // Schema check.
        let mut bad = draft.clone();
        bad.schema_id = SchemaId::new("test/unregistered".into());
        let err = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                bad,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::UnknownSchema);

        // Counts.
        let memories: (i64,) = sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.memories")
            .fetch_one(pg.pool_for_tests())
            .await?;
        assert_eq!(memories.0, 1);

        let change: (i64,) =
            sqlx::query_as("SELECT count(*)::bigint FROM proxima_core.change_event")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(change.0, 1);

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("fact_ingest_pg test failed");
}

#[tokio::test]
async fn list_change_events_for_replay_respects_bounds_and_owner() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let other_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        let first = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                fresh_draft(owner),
            )
            .await?;
        let second = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(
                    &owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                fresh_draft(owner),
            )
            .await?;
        let other_permit = owner_write_permit(&other_owner, proxima_core::AccessKind::Fact).await?;
        pg.ingest_fact_atomic(&other_permit, &fresh_draft(other_owner), None)
            .await?;

        let rows = pg
            .list_change_events_for_replay(
                &owner,
                first.change_event_seq,
                Some(second.change_event_seq),
                10,
            )
            .await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event.seq, second.change_event_seq);
        assert_eq!(rows[0].event.owner, owner);

        let rows = pg
            .list_change_events_for_replay(&owner, Uuid::nil(), None, 1)
            .await?;
        assert_eq!(rows.len(), 1, "limit applies to replay scan");
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("replay scan bounds failed");
}

/// The `change_event` pull scopes by principal — matching the memories read
/// path (`query_owner_scope_is_principal`) and the event-history scan. Owner is
/// the principal (org left Core); a harness re-deriving the same
/// principal must see its events.
#[tokio::test]
async fn list_change_events_after_scopes_by_principal() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();

        let principal = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let stored_owner = principal;
        let requested_owner = principal;

        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        let ingested = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(
                    &stored_owner,
                    proxima_core::AuthPath::HostBearer,
                ),
                fresh_draft(stored_owner),
            )
            .await?;

        // Pull under the same owner: the event is returned, scoped by
        // principal alone.
        let rows = pg
            .list_change_events_after(std::slice::from_ref(&requested_owner), Uuid::nil(), 10)
            .await?;
        assert_eq!(rows.len(), 1, "pull scopes by principal");
        assert_eq!(rows[0].event.seq, ingested.change_event_seq);
        assert_eq!(rows[0].event.owner, stored_owner);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("change_event pull principal-scoping failed");
}

#[tokio::test]
async fn list_change_events_after_filters_by_read_owners() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        let p = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let q = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let g1 = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        seed_membership(pg.pool_for_tests(), &g1, &q, Relation::Viewer).await?;

        let group_authz = proxima_core::AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(g1, Role::admin())],
            proxima_core::AuthPath::HostBearer,
        )
        .narrowed_to_owner(g1)
        .expect("admin role narrows to target owner");
        let group_event = engine.fact_ingest(&group_authz, fresh_draft(g1)).await?;
        let p_event = engine
            .fact_ingest(
                &proxima_core::AuthzContext::single_owner(&p, proxima_core::AuthPath::HostBearer),
                fresh_draft(p),
            )
            .await?;

        let q_read_owners = vec![q, g1];
        let rows = pg
            .list_change_events_after(&q_read_owners, Uuid::nil(), 10)
            .await?;
        let seqs = rows.iter().map(|row| row.event.seq).collect::<Vec<Uuid>>();
        assert!(seqs.contains(&group_event.change_event_seq));
        assert!(
            !seqs.contains(&p_event.change_event_seq),
            "Q must not see P's personal event"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("change_event pull read-owner filtering failed");
}

const ASCENDING_ORDER_EVENT_COUNT: usize = 6;

/// `hydrate_change_events_batch` always returns rows ordered by `seq DESC`;
/// `list_change_events_after` must restore ascending order for wake
/// consumers regardless of that internal ordering (batch-hydrate
/// migration off the old per-row `hydrate_change_event` loop).
#[tokio::test]
async fn list_change_events_after_preserves_ascending_seq_order() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let storage = Arc::new(pg.clone()).storage_ports();
        let registry = FlavorRegistryFrozen::with_schemas(schemas_for_test());
        let engine = Engine::new(registry).with_storage_ports(storage);

        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let authz =
            proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::HostBearer);

        let mut expected_seqs = Vec::with_capacity(ASCENDING_ORDER_EVENT_COUNT);
        for _ in 0..ASCENDING_ORDER_EVENT_COUNT {
            let outcome = engine.fact_ingest(&authz, fresh_draft(owner)).await?;
            expected_seqs.push(outcome.change_event_seq);
        }
        expected_seqs.sort_unstable();

        let rows = pg
            .list_change_events_after(std::slice::from_ref(&owner), Uuid::nil(), 100)
            .await?;
        let seqs: Vec<Uuid> = rows.iter().map(|row| row.event.seq).collect();

        assert_eq!(
            seqs.len(),
            ASCENDING_ORDER_EVENT_COUNT,
            "all ingested events must be returned"
        );
        assert_eq!(
            seqs, expected_seqs,
            "list_change_events_after must return ascending seq order"
        );
        assert!(
            seqs.windows(2).all(|w| w[0] < w[1]),
            "seqs must be strictly ascending: {seqs:?}"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("list_change_events_after ascending-order test failed");
}

async fn seed_membership(
    pool: &sqlx::PgPool,
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
    .execute(pool)
    .await?;
    Ok(())
}
