//! Owner-registry and membership writes can participate in a host transaction.

use std::borrow::Cow;
use std::sync::LazyLock;

use proxima::flavor::NamedMigrator;
use proxima::testkit::{HostTemplate, SplitRoleDb, TemplateFamily};
use proxima::{GroupId, OwnerId, Relation, UserId};
use proxima_core::storage_ports::OwnerMembershipAdminPort;
use proxima_storage_pg::{PgPlatformScope, PgStorage};
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::{AssertSqlSafe, PgPool, SqlSafeStr};
use uuid::Uuid;

const HOST_MIGRATION: i64 = 20_991_014_000_001;

fn migration() -> Migration {
    Migration::new(
        HOST_MIGRATION,
        Cow::Borrowed("host membership transaction table"),
        MigrationType::Simple,
        AssertSqlSafe(
            "CREATE TABLE public.host_membership_transaction_rows (id uuid PRIMARY KEY)".to_owned(),
        )
        .into_sql_str(),
        false,
    )
}

fn host_migrator() -> NamedMigrator {
    NamedMigrator::flavor(
        "host-membership-transaction",
        Migrator {
            migrations: Cow::Owned(vec![migration()]),
            ..Migrator::DEFAULT
        },
    )
}

static TEMPLATE: LazyLock<HostTemplate> = LazyLock::new(|| {
    HostTemplate::new(
        TemplateFamily::new("proxima_host_membership_tx_").expect("template family"),
        "host-membership-transaction-v1",
        vec![host_migrator()],
    )
    .expect("host migrator lineup")
});

async fn clone_db() -> SplitRoleDb {
    TEMPLATE
        .clone_split_role("proxima_host_membership_tx", &[])
        .await
        .expect("PG required")
}

async fn platform_scope(db: &SplitRoleDb) -> PgPlatformScope {
    PgPlatformScope::new(
        PgPool::connect(db.platform_url())
            .await
            .expect("platform pool"),
        &["proxima_core"],
    )
    .await
    .expect("platform scope")
}

async fn host_row_count(db: &SplitRoleDb) -> i64 {
    let scope = platform_scope(db).await;
    let mut tx = scope.begin().await.expect("platform transaction");
    let count = sqlx::query_scalar("SELECT count(*) FROM public.host_membership_transaction_rows")
        .fetch_one(&mut *tx)
        .await
        .expect("host rows");
    tx.rollback().await.expect("count rollback");
    count
}

async fn owner_and_membership_counts(
    db: &SplitRoleDb,
    group: GroupId,
    admin: UserId,
) -> (i64, i64) {
    let scope = platform_scope(db).await;
    let mut tx = scope.begin().await.expect("platform transaction");
    let owners = sqlx::query_scalar("SELECT count(*) FROM proxima_core.owners WHERE owner_id = $1")
        .bind(group.into_inner())
        .fetch_one(&mut *tx)
        .await
        .expect("owner row");
    let members = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.group_memberships
          WHERE group_id = $1 AND member_user_id = $2",
    )
    .bind(group.into_inner())
    .bind(admin.into_inner())
    .fetch_one(&mut *tx)
    .await
    .expect("membership rows");
    tx.rollback().await.expect("count rollback");
    (owners, members)
}

#[tokio::test]
async fn host_transaction_rolls_back_registry_membership_and_host_row() {
    let db = clone_db().await;
    let group = GroupId::new(Uuid::now_v7());
    let admin = UserId::new(Uuid::now_v7());
    let host_row = Uuid::now_v7();
    let scope = platform_scope(&db).await;
    let mut tx = scope.begin().await.expect("platform transaction");

    proxima::register_group(&mut tx, group)
        .await
        .expect("register group");
    proxima::bootstrap_group_admin(&mut tx, group, admin)
        .await
        .expect("first admin");
    sqlx::query("INSERT INTO public.host_membership_transaction_rows (id) VALUES ($1)")
        .bind(host_row)
        .execute(&mut *tx)
        .await
        .expect("host row");
    tx.rollback().await.expect("rollback");

    assert_eq!(owner_and_membership_counts(&db, group, admin).await, (0, 0));
    assert_eq!(host_row_count(&db).await, 0);
}

#[tokio::test]
async fn committed_host_transaction_keeps_membership_and_role_probe_sees_admin() {
    let db = clone_db().await;
    let group = GroupId::new(Uuid::now_v7());
    let admin = UserId::new(Uuid::now_v7());
    let scope = platform_scope(&db).await;
    let mut tx = scope.begin().await.expect("platform transaction");

    proxima::register_group(&mut tx, group)
        .await
        .expect("register group");
    proxima::bootstrap_group_admin(&mut tx, group, admin)
        .await
        .expect("first admin");
    sqlx::query("INSERT INTO public.host_membership_transaction_rows (id) VALUES ($1)")
        .bind(Uuid::now_v7())
        .execute(&mut *tx)
        .await
        .expect("host row");
    tx.commit().await.expect("commit");

    assert_eq!(owner_and_membership_counts(&db, group, admin).await, (1, 1));
    assert_eq!(host_row_count(&db).await, 1);
    let mut probe = scope.begin().await.expect("probe transaction");
    let roles = proxima::roles_for_subject(&mut probe, admin)
        .await
        .expect("role probe");
    assert_eq!(
        roles.role_for(&OwnerId::from(group)),
        Some(proxima::Role::admin())
    );
    probe.rollback().await.expect("probe rollback");
}

#[tokio::test]
async fn connection_membership_write_keeps_user_registry_constraint() {
    let db = clone_db().await;
    let user_id = UserId::new(Uuid::now_v7());
    let group = GroupId::new(user_id.into_inner());
    let scope = platform_scope(&db).await;
    let mut tx = scope.begin().await.expect("platform transaction");
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(user_id.into_inner())
        .execute(&mut *tx)
        .await
        .expect("register personal owner");
    tx.commit().await.expect("commit personal owner");

    let mut tx = scope.begin().await.expect("membership transaction");
    let error = proxima::add_group_member(
        &mut tx,
        group,
        UserId::new(Uuid::now_v7()).into(),
        Relation::Viewer,
    )
    .await
    .expect_err("a user id cannot become a group");
    assert!(
        matches!(error, proxima::StorageError::Conflict(_)),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("group_memberships_group_registered_fk"),
        "the database constraint must be named in the refusal: {error}"
    );
    tx.rollback().await.expect("rollback refused membership");

    let storage = PgStorage::connect_with_config(
        db.runtime_url(),
        proxima_storage_pg::PgPoolConfig::from_env().expect("pool config"),
        proxima_storage_pg::PgTuning::from_env().expect("pool tuning"),
        scope,
    )
    .await
    .expect("pool storage");
    let pool_error = storage
        .bootstrap_group_admin(group, UserId::new(Uuid::now_v7()), Uuid::now_v7())
        .await
        .expect_err("the pool form must retain the same constraint refusal");
    assert!(
        matches!(pool_error, proxima::StorageError::Conflict(_)),
        "pool and connection forms must both refuse a personal owner as group: {pool_error}"
    );
    assert_eq!(
        pool_error.to_string(),
        error.to_string(),
        "same database refusal"
    );
    storage.pool_for_tests().close().await;
}

#[tokio::test]
async fn connection_relation_writes_share_the_host_transaction() {
    let db = clone_db().await;
    let group = GroupId::new(Uuid::now_v7());
    let member = UserId::new(Uuid::now_v7());
    let scope = platform_scope(&db).await;
    let mut tx = scope.begin().await.expect("platform transaction");

    proxima::register_group(&mut tx, group)
        .await
        .expect("register group");
    proxima::add_group_member(&mut tx, group, member.into(), Relation::Viewer)
        .await
        .expect("add viewer");
    assert_eq!(
        proxima::group_role(&mut tx, member, group)
            .await
            .expect("viewer probe"),
        Some(proxima::Role::viewer())
    );
    tx.commit().await.expect("commit existing viewer");
    let mut tx = scope.begin().await.expect("relation mutation transaction");
    proxima::replace_group_member_relation(
        &mut tx,
        group,
        member.into(),
        Relation::Viewer,
        Relation::Editor,
    )
    .await
    .expect("replace relation");
    assert_eq!(
        proxima::group_role(&mut tx, member, group)
            .await
            .expect("editor probe"),
        Some(proxima::Role::editor())
    );
    proxima::remove_group_member_relation(&mut tx, group, member.into(), Relation::Editor)
        .await
        .expect("remove relation");
    assert_eq!(
        proxima::group_role(&mut tx, member, group)
            .await
            .expect("removed probe"),
        None
    );
    proxima::add_group_member(&mut tx, group, member.into(), Relation::Admin)
        .await
        .expect("add admin");
    assert_eq!(
        proxima::group_role(&mut tx, member, group)
            .await
            .expect("admin probe"),
        Some(proxima::Role::admin())
    );
    proxima::remove_group_member(&mut tx, group, member.into())
        .await
        .expect("remove member");
    tx.rollback().await.expect("rollback relation mutations");
    let mut verify = scope.begin().await.expect("verify rollback transaction");
    assert_eq!(
        proxima::group_role(&mut verify, member, group)
            .await
            .expect("restored viewer probe"),
        Some(proxima::Role::viewer())
    );
    proxima::remove_group_member(&mut verify, group, member.into())
        .await
        .expect("remove member for commit");
    verify.commit().await.expect("commit removal");

    let mut verify = scope.begin().await.expect("verify transaction");
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.group_memberships
          WHERE group_id = $1 AND member_user_id = $2",
    )
    .bind(group.into_inner())
    .bind(member.into_inner())
    .fetch_one(&mut *verify)
    .await
    .expect("membership count");
    assert_eq!(count, 0);
    verify.rollback().await.expect("verify rollback");
}
