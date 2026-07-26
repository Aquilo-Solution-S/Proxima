//! Erase refusals and their audit trail: content-free outcomes, World owners, and live group members.

use super::{admin_authz_for, audit_count, compliance_engine, seed_group_member};

use crate::common::{create_db, db_url, drop_db};
use proxima_core::{
    AuthPath, AuthzContext, ComplianceEraseCounts, ComplianceEraseOutcome, ComplianceEraseRefusal,
    GroupId, OwnerRef, Relation, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[test]
fn compliance_outcome_counts_are_content_free() {
    let counts = ComplianceEraseCounts {
        memories: 1,
        goals: 2,
        edges: 3,
        fact_entities: 4,
        receipts: 5,
        source_batches: 6,
        citations: 7,
        cited_objects: 8,
        source_cursors: 15,
        embeddings: 9,
        embedding_jobs: 10,
        mcp_call_rows: 11,
        change_events: 12,
        redacted_edge_targets: 13,
        suppressed_keys: 14,
    };
    let outcome = ComplianceEraseOutcome::Completed {
        operation_id: Uuid::now_v7(),
        counts,
        cited_object_purge_pending: false,
    };
    assert!(matches!(
        outcome,
        ComplianceEraseOutcome::Completed {
            counts: ComplianceEraseCounts { memories: 1, .. },
            ..
        }
    ));
}

#[test]
fn world_owner_refusal_has_a_typed_reason() {
    let outcome = ComplianceEraseOutcome::Refused {
        operation_id: Uuid::now_v7(),
        reason: ComplianceEraseRefusal::WorldOwner,
    };
    assert!(matches!(
        outcome,
        ComplianceEraseOutcome::Refused {
            reason: ComplianceEraseRefusal::WorldOwner,
            ..
        }
    ));
}

#[tokio::test]
async fn group_owner_with_member_is_refused_and_audited() -> Result<(), Box<dyn std::error::Error>>
{
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let group = GroupId::new(Uuid::now_v7());
        let member = UserId::new(Uuid::now_v7());
        seed_group_member(&pg, group, member, Relation::Admin).await?;
        let authz = AuthzContext::for_subject(member, AuthPath::HostBearer);

        let outcome = engine.erase_abandoned_group_owner(&authz, group).await?;
        assert!(matches!(
            outcome,
            ComplianceEraseOutcome::Refused {
                reason: ComplianceEraseRefusal::OwnerNotAbandoned,
                ..
            }
        ));
        assert_eq!(audit_count(&pg).await?, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn world_owner_erase_refuses_and_audits() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);

        let outcome = engine
            .erase_world_owner(&AuthzContext::for_subject(
                UserId::new(Uuid::now_v7()),
                AuthPath::HostBearer,
            ))
            .await?;
        assert!(matches!(
            outcome,
            ComplianceEraseOutcome::Refused {
                reason: ComplianceEraseRefusal::WorldOwner,
                ..
            }
        ));
        assert_eq!(audit_count(&pg).await?, 1);
        let target_kind: String =
            sqlx::query_scalar("SELECT target_kind FROM proxima_core.compliance_audit_log LIMIT 1")
                .fetch_one(pg.pool_for_tests())
                .await?;
        assert_eq!(target_kind, "WorldOwner");

        let held_owner = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        engine
            .set_legal_hold(&admin_authz_for(held_owner), &held_owner)
            .await?;
        let held_outcome = engine
            .erase_world_owner(&AuthzContext::for_subject(
                UserId::new(Uuid::now_v7()),
                AuthPath::HostBearer,
            ))
            .await?;
        assert!(matches!(
            held_outcome,
            ComplianceEraseOutcome::Refused {
                reason: ComplianceEraseRefusal::WorldOwner,
                ..
            }
        ));
        assert_eq!(audit_count(&pg).await?, 2);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}
