//! Source-scoped erase: scope containment, shared fact entities, and source cursors.

use super::{
    assert_shared_fact_entity_survives_source_scope_erase, compliance_engine, fact_permit,
    receipt_draft, seed_fact, seed_shared_fact_entity_fixture,
};

use crate::common::{create_db, db_url, drop_db, seed_memory_edge};
use proxima_core::change_event::EdgeTargetProjection;
use proxima_core::storage_ports::{EdgeReadPort, FactIngestPort};
use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest};
use proxima_core::{
    AuthPath, AuthzContext, ComplianceEraseOutcome, EdgeId, EntityKind, GroupId, OwnerRef,
    RelationClass, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[tokio::test]
async fn group_source_scope_erases_only_requested_source_and_suppresses_new_batches()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let erased_draft = receipt_draft("test/source-a", Uuid::now_v7(), b"erase-source-a");
        let kept_draft = receipt_draft("test/source-b", Uuid::now_v7(), b"keep-source-b");
        let erased = seed_fact(&pg, &owner, &erased_draft).await?;
        let kept = seed_fact(&pg, &owner, &kept_draft).await?;
        let surviving_edge = seed_memory_edge(
            &pg,
            &owner,
            (EntityKind::Fact, kept.memory_id),
            (EntityKind::Fact, erased.memory_id),
            "test/compliance/source-scope-mentions",
            RelationClass::Structural,
        )
        .await?;

        let outcome = engine
            .erase_abandoned_group_source_scope(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
                SourceId::new("test/source-a"),
            )
            .await?;
        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));

        let erased_remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(erased.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        let kept_remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(kept.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(erased_remaining, 0);
        assert_eq!(kept_remaining, 1);
        let edge_rows = pg
            .read_edges(
                &[owner],
                &EdgeReadRequest {
                    owner,
                    edge_ids: vec![EdgeId::new(surviving_edge.into_inner())],
                    filter: EdgeFilter::default(),
                    limit: 10,
                    cursor: None,
                    include_payloads: false,
                },
                &[],
            )
            .await?;
        assert_eq!(edge_rows.edges.len(), 1);
        assert_eq!(edge_rows.edges[0].target, EdgeTargetProjection::Unavailable);

        let replay = receipt_draft("test/source-a", Uuid::now_v7(), b"new-source-a");
        let suppressed = pg
            .ingest_fact_atomic(&fact_permit(&owner).await?, &replay, None)
            .await
            .expect_err("owner/source suppression blocks new batches before dedup");
        assert!(matches!(
            suppressed,
            proxima_core::StorageError::Suppressed(_)
        ));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn source_scope_erase_preserves_shared_fact_entity_head_and_source_edges()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let fixture = seed_shared_fact_entity_fixture(&pg, owner).await?;

        let outcome = engine
            .erase_abandoned_group_source_scope(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
                SourceId::new("test/shared-entity-a"),
            )
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed source-scope erase");
        };
        assert_eq!(counts.fact_entities, 0, "shared fact entity must survive");

        assert_shared_fact_entity_survives_source_scope_erase(&pg, &fixture).await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn personal_source_scope_with_verified_drop_erases_only_scope()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let erased_draft = receipt_draft("personal/source-a", Uuid::now_v7(), b"erase-personal-a");
        let kept_draft = receipt_draft("personal/source-b", Uuid::now_v7(), b"keep-personal-b");
        let erased = seed_fact(&pg, &owner, &erased_draft).await?;
        let kept = seed_fact(&pg, &owner, &kept_draft).await?;

        let outcome = engine
            .erase_dropped_personal_source_scope(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                user,
                SourceId::new("personal/source-a"),
                "drop-ok".to_owned(),
            )
            .await?;
        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));
        let counts: (i64, i64) = sqlx::query_as(
            "SELECT
                count(*) FILTER (WHERE memory_id = $1)::bigint,
                count(*) FILTER (WHERE memory_id = $2)::bigint
               FROM proxima_core.memories",
        )
        .bind(erased.memory_id.into_inner())
        .bind(kept.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(counts, (0, 1));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// An owner erase physically
/// removes the owner's persisted projector cursors and counts them, so a
/// re-provisioned owner never resumes from a stale offset.
#[tokio::test]
async fn abandoned_group_owner_erase_removes_source_cursors()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let group_uuid = Uuid::now_v7();
        let group = GroupId::new(group_uuid);
        let owner = OwnerRef::Group(group);
        let authz = AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer);
        let draft = receipt_draft("test/source", Uuid::now_v7(), b"erase-me");
        seed_fact(&pg, &owner, &draft).await?;

        sqlx::query(
            "INSERT INTO proxima_core.source_cursors (owner_kind, owner_id, source, cursor)
             VALUES ('group', $1, 'test/source', $2)",
        )
        .bind(group_uuid)
        .bind(&b"opaque-offset"[..])
        .execute(pg.pool_for_tests())
        .await?;

        let outcome = engine.erase_abandoned_group_owner(&authz, group).await?;
        let ComplianceEraseOutcome::Completed {
            operation_id,
            counts,
            ..
        } = outcome
        else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(
            counts.source_cursors, 1,
            "owner erase counts the deleted cursor"
        );

        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.source_cursors WHERE owner_id = $1",
        )
        .bind(group_uuid)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            remaining, 0,
            "the cursor is physically erased with the owner"
        );

        let persisted: i64 = sqlx::query_scalar(
            "SELECT source_cursors_count FROM proxima_core.compliance_audit_log
              WHERE operation_id = $1",
        )
        .bind(operation_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            persisted, 1,
            "the durable audit row records the cursor erasure count"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}
