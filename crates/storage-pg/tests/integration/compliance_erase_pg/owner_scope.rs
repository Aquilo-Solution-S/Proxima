//! Owner-scoped erase: abandoned owners, surviving source edges, and planned purges.

use super::{
    compliance_engine, compliance_engine_with_failing_purge, fact_permit, receipt_draft,
    seed_delegation_grant, seed_fact,
};

use crate::common::{create_db, db_url, drop_db, seed_memory, seed_memory_edge};
use proxima_core::storage_ports::{ComplianceErasePort, EdgeReadPort, FactIngestPort};
use proxima_core::verbs::query::{EdgeFilter, EdgeReadRequest};
use proxima_core::{
    AuthPath, AuthzContext, ComplianceEraseOutcome, EdgeKind, EdgeTargetProjection, EntityKind,
    EntityRef, GroupId, OwnerRef, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[tokio::test]
async fn abandoned_group_owner_erases_owned_fact_and_suppresses_reingest()
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
        let authz = AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer);
        let draft = receipt_draft("test/source", Uuid::now_v7(), b"erase-me");
        let first = seed_fact(&pg, &owner, &draft).await?;

        let outcome = engine.erase_abandoned_group_owner(&authz, group).await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.memories, 1);
        assert!(counts.receipts >= 1);
        assert!(counts.suppressed_keys >= 1);

        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(first.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(remaining, 0);

        let suppressed = pg
            .ingest_fact_atomic(&fact_permit(&owner).await?, &draft, None)
            .await
            .expect_err("suppression must block reingest before receipt replay");
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
async fn target_abandoned_keeps_live_source_edge_as_unavailable()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let live = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let group = GroupId::new(Uuid::now_v7());
        let target_owner = OwnerRef::Group(group);
        let source = seed_memory(&pg, &live, EntityKind::Fact, "source").await?;
        let target = seed_memory(&pg, &target_owner, EntityKind::Fact, "target").await?;
        seed_memory_edge(
            &pg,
            &live,
            (EntityKind::Fact, source),
            (EntityKind::Fact, target),
            EdgeKind::Reference,
        )
        .await?;

        let outcome = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
            )
            .await?;
        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));

        let edge_rows: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.edges WHERE source_id = $1",
        )
        .bind(source.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(edge_rows, 1, "live source-owned edge row survives");

        // The redaction is keyed by the edge, and the edge is its own key.
        let redactions: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM proxima_core.compliance_edge_target_redactions
              WHERE source_id = $1 AND target_id = $2",
        )
        .bind(source.into_inner())
        .bind(target.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(redactions, 1);

        let read = pg
            .read_edges(
                &[live],
                &EdgeReadRequest {
                    owner: live,
                    filter: EdgeFilter {
                        kind: None,
                        source: Some(EntityRef::Memory(source)),
                        target: None,
                    },
                    limit: 10,
                    cursor: None,
                },
            )
            .await?;
        assert_eq!(read.edges.len(), 1);
        assert_eq!(read.edges[0].target, EdgeTargetProjection::Unavailable);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// GDPR audit-truth contract: an owner-scope erase with a planned cited-
/// object purge persists `cited_object_purge_pending = true` on the audit
/// row in the SAME transaction as the erase (independent of whether the
/// purge itself later succeeds), and the dedicated clear verb is the only
/// thing that flips it back to false.
#[tokio::test]
async fn owner_erase_with_planned_purge_persists_pending_until_cleared()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine_with_failing_purge(&pg);
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let authz = AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer);
        seed_fact(
            &pg,
            &owner,
            &receipt_draft("test/purge-source", Uuid::now_v7(), b"purge-me"),
        )
        .await?;

        let outcome = engine.erase_abandoned_group_owner(&authz, group).await?;
        let ComplianceEraseOutcome::Completed {
            operation_id,
            cited_object_purge_pending,
            ..
        } = outcome
        else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert!(
            cited_object_purge_pending,
            "a failed purge must surface pending on the returned outcome"
        );

        let persisted: bool = sqlx::query_scalar(
            "SELECT cited_object_purge_pending FROM proxima_core.compliance_audit_log
              WHERE operation_id = $1",
        )
        .bind(operation_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            persisted,
            "the durable audit row must persist pending=true when the purge fails"
        );

        pg.clear_cited_object_purge_pending(operation_id).await?;

        let cleared: bool = sqlx::query_scalar(
            "SELECT cited_object_purge_pending FROM proxima_core.compliance_audit_log
              WHERE operation_id = $1",
        )
        .bind(operation_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            !cleared,
            "the clear verb must flip the durable flag back to false"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn group_owner_erase_removes_only_exact_owner_delegations_and_audits_count()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let erased_group = GroupId::new(Uuid::now_v7());
        let kept_group = GroupId::new(Uuid::now_v7());
        let subject = UserId::new(Uuid::now_v7());
        let erased_grant =
            seed_delegation_grant(&pg, OwnerRef::Group(erased_group), subject).await?;
        let kept_grant = seed_delegation_grant(&pg, OwnerRef::Group(kept_group), subject).await?;

        let outcome = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(subject, AuthPath::HostBearer),
                erased_group,
            )
            .await?;
        let ComplianceEraseOutcome::Completed {
            operation_id,
            counts,
            ..
        } = outcome
        else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.delegated_authority_grants, 1);

        let remaining: (i64, i64) = sqlx::query_as(
            "SELECT
                 count(*) FILTER (WHERE delegation_id = $1)::bigint,
                 count(*) FILTER (WHERE delegation_id = $2)::bigint
               FROM proxima_core.delegated_authority_grants",
        )
        .bind(erased_grant)
        .bind(kept_grant)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(remaining, (0, 1));

        let audited: i64 = sqlx::query_scalar(
            "SELECT delegated_authority_grants_count
               FROM proxima_core.compliance_audit_log
              WHERE operation_id = $1",
        )
        .bind(operation_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(audited, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn personal_owner_erase_removes_owned_and_cross_owner_subject_delegations()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let erased_user = UserId::new(Uuid::now_v7());
        let other_user = UserId::new(Uuid::now_v7());
        let group_owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let owned =
            seed_delegation_grant(&pg, OwnerRef::Personal(erased_user), erased_user).await?;
        let cross_owner = seed_delegation_grant(&pg, group_owner, erased_user).await?;
        let kept = seed_delegation_grant(&pg, group_owner, other_user).await?;

        let outcome = engine
            .erase_dropped_personal_owner(
                &AuthzContext::for_subject(erased_user, AuthPath::HostBearer),
                erased_user,
                "drop-ok".to_owned(),
            )
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed erase, got {outcome:?}");
        };
        assert_eq!(counts.delegated_authority_grants, 2);

        let remaining: (i64, i64, i64) = sqlx::query_as(
            "SELECT
                 count(*) FILTER (WHERE delegation_id = $1)::bigint,
                 count(*) FILTER (WHERE delegation_id = $2)::bigint,
                 count(*) FILTER (WHERE delegation_id = $3)::bigint
               FROM proxima_core.delegated_authority_grants",
        )
        .bind(owned)
        .bind(cross_owner)
        .bind(kept)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(remaining, (0, 0, 1));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}
