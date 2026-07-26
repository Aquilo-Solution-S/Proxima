//! Legal hold: round trips, the admin read/write split, and what it does and does not block.

use super::{
    admin_authz_for, assert_legal_hold_refusal, compliance_engine, engine_without_compliance_admin,
    owner_content_counts, receipt_draft, seed_fact,
};

use crate::common::{create_db, db_url, drop_db};
use proxima_core::access::Role;
use proxima_core::verbs::query::QueryRequest;
use proxima_core::{
    AuthPath, AuthzContext, ComplianceEraseOutcome, GroupId, OwnerRef, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

#[tokio::test]
async fn legal_hold_round_trips_and_set_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let authz = admin_authz_for(owner);

        assert!(!engine.get_legal_hold(&authz, &owner).await?);
        engine.set_legal_hold(&authz, &owner).await?;
        assert!(engine.get_legal_hold(&authz, &owner).await?);
        engine.set_legal_hold(&authz, &owner).await?;
        assert!(engine.get_legal_hold(&authz, &owner).await?);
        assert!(engine.clear_legal_hold(&authz, &owner).await?);
        assert!(!engine.get_legal_hold(&authz, &owner).await?);
        assert!(!engine.clear_legal_hold(&authz, &owner).await?);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn owner_admin_can_read_but_not_set_or_clear_legal_hold()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = engine_without_compliance_admin(&pg);
        let operator_engine = compliance_engine(&pg);
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let owner_admin = AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        );
        let operator = admin_authz_for(owner);

        assert!(!engine.get_legal_hold(&owner_admin, &owner).await?);
        let set_err = engine
            .set_legal_hold(&owner_admin, &owner)
            .await
            .expect_err("owner admin without operator authority cannot set hold");
        assert_eq!(set_err.code, proxima_core::ErrorCode::Forbidden);
        assert!(
            !engine.get_legal_hold(&owner_admin, &owner).await?,
            "denied set must leave hold inactive"
        );

        operator_engine.set_legal_hold(&operator, &owner).await?;
        assert!(engine.get_legal_hold(&owner_admin, &owner).await?);
        let clear_err = engine
            .clear_legal_hold(&owner_admin, &owner)
            .await
            .expect_err("owner admin without operator authority cannot clear hold");
        assert_eq!(clear_err.code, proxima_core::ErrorCode::Forbidden);
        assert!(
            engine.get_legal_hold(&owner_admin, &owner).await?,
            "denied clear must leave hold active"
        );

        assert!(operator_engine.clear_legal_hold(&operator, &owner).await?);
        assert!(!engine.get_legal_hold(&owner_admin, &owner).await?);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn legal_hold_blocks_destructive_erase_verbs_without_deleting_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);

        let group = GroupId::new(Uuid::now_v7());
        let group_owner = OwnerRef::Group(group);
        seed_fact(
            &pg,
            &group_owner,
            &receipt_draft("hold/group-a", Uuid::now_v7(), b"held-group-a"),
        )
        .await?;
        seed_fact(
            &pg,
            &group_owner,
            &receipt_draft("hold/group-b", Uuid::now_v7(), b"held-group-b"),
        )
        .await?;
        let group_authz = admin_authz_for(group_owner);
        engine.set_legal_hold(&group_authz, &group_owner).await?;
        let group_counts = owner_content_counts(&pg, group_owner).await?;

        let outcome = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
            )
            .await?;
        assert_legal_hold_refusal(&outcome);
        assert_eq!(owner_content_counts(&pg, group_owner).await?, group_counts);

        let outcome = engine
            .erase_abandoned_group_source_scope(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
                SourceId::new("hold/group-a"),
            )
            .await?;
        assert_legal_hold_refusal(&outcome);
        assert_eq!(owner_content_counts(&pg, group_owner).await?, group_counts);

        let user = UserId::new(Uuid::now_v7());
        let personal_owner = OwnerRef::Personal(user);
        seed_fact(
            &pg,
            &personal_owner,
            &receipt_draft("hold/personal-a", Uuid::now_v7(), b"held-personal-a"),
        )
        .await?;
        seed_fact(
            &pg,
            &personal_owner,
            &receipt_draft("hold/personal-b", Uuid::now_v7(), b"held-personal-b"),
        )
        .await?;
        let personal_authz = admin_authz_for(personal_owner);
        engine
            .set_legal_hold(&personal_authz, &personal_owner)
            .await?;
        let personal_counts = owner_content_counts(&pg, personal_owner).await?;

        let outcome = engine
            .erase_dropped_personal_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                user,
                "drop-ok".to_owned(),
            )
            .await?;
        assert_legal_hold_refusal(&outcome);
        assert_eq!(
            owner_content_counts(&pg, personal_owner).await?,
            personal_counts
        );

        let outcome = engine
            .erase_dropped_personal_source_scope(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                user,
                SourceId::new("hold/personal-a"),
                "drop-ok".to_owned(),
            )
            .await?;
        assert_legal_hold_refusal(&outcome);
        assert_eq!(
            owner_content_counts(&pg, personal_owner).await?,
            personal_counts
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn legal_hold_does_not_block_reads_or_ordinary_writes()
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
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        engine.set_legal_hold(&authz, &owner).await?;

        let written = engine
            .fact_ingest(
                &authz,
                receipt_draft("hold/write", Uuid::now_v7(), b"write-while-held"),
            )
            .await?;
        let read = engine
            .query(&authz, &QueryRequest::for_owner(owner))
            .await?;
        assert!(read.memories.iter().any(|row| row.id == written.memory_id));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn clearing_legal_hold_restores_prior_erase_behavior()
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
        let written = seed_fact(
            &pg,
            &owner,
            &receipt_draft("hold/clear", Uuid::now_v7(), b"clear-then-erase"),
        )
        .await?;
        let authz = admin_authz_for(owner);
        engine.set_legal_hold(&authz, &owner).await?;

        let refused = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
            )
            .await?;
        assert_legal_hold_refusal(&refused);

        assert!(engine.clear_legal_hold(&authz, &owner).await?);
        let completed = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
            )
            .await?;
        assert!(matches!(
            completed,
            ComplianceEraseOutcome::Completed { .. }
        ));
        let remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(written.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(remaining, 0);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn legal_hold_on_one_owner_does_not_affect_another_owner_erase()
-> Result<(), Box<dyn std::error::Error>> {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&db_name).await?;
    let url = db_url(&db_name);
    let result = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let engine = compliance_engine(&pg);
        let held_group = GroupId::new(Uuid::now_v7());
        let held_owner = OwnerRef::Group(held_group);
        seed_fact(
            &pg,
            &held_owner,
            &receipt_draft("hold/owner-a", Uuid::now_v7(), b"held-owner-a"),
        )
        .await?;
        let held_counts = owner_content_counts(&pg, held_owner).await?;
        engine
            .set_legal_hold(&admin_authz_for(held_owner), &held_owner)
            .await?;

        let free_group = GroupId::new(Uuid::now_v7());
        let free_owner = OwnerRef::Group(free_group);
        let free = seed_fact(
            &pg,
            &free_owner,
            &receipt_draft("hold/owner-b", Uuid::now_v7(), b"free-owner-b"),
        )
        .await?;
        let outcome = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                free_group,
            )
            .await?;
        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));
        let free_remaining: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(free.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(free_remaining, 0);
        assert_eq!(owner_content_counts(&pg, held_owner).await?, held_counts);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}
