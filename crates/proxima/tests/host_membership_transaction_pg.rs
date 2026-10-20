//! Owner-registry and membership writes can participate in a host transaction.

use std::borrow::Cow;
use std::sync::LazyLock;
use std::time::Duration;

use proxima::flavor::NamedMigrator;
use proxima::testkit::{HostTemplate, SplitRoleDb, TemplateFamily};
use proxima::{
    AuthorityTargets, GroupContainment, GroupId, MembershipAddOutcome, OwnerId, RegisteredOwner,
    Relation, Role, StorageError, UserId,
};
use proxima_core::storage_ports::OwnerMembershipAdminPort;
use proxima_storage_pg::{PgPlatformScope, PgStorage};
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::{AssertSqlSafe, PgPool, SqlSafeStr};
use uuid::Uuid;

const HOST_MIGRATION: i64 = 20_991_014_000_001;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

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
        proxima::PgSidecarRegistryFrozen::default(),
    )
    .expect("host migrator lineup")
});

async fn clone_db() -> SplitRoleDb {
    let db = TEMPLATE
        .clone_split_role("proxima_host_membership_tx", &[])
        .await
        .expect("PG required");
    let migration = proxima_storage_pg::PgMigrationStorage::connect_with_config(
        db.platform_url(),
        proxima_storage_pg::PgPoolConfig::default(),
        proxima_storage_pg::PgTuning::default(),
    )
    .await
    .expect("migration pool");
    migration
        .initialize_store_number(0)
        .await
        .expect("fixture store number");
    db
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
        0,
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

async fn wait_for_authority_lock(pool: &PgPool, pid: i32) -> TestResult {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks
                  WHERE pid = $1 AND locktype = 'advisory' AND NOT granted)",
            )
            .bind(pid)
            .fetch_one(pool)
            .await?;
            if waiting {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "direct and inherited facts share one uncommitted host transaction"
)]
async fn authority_reads_direct_relations_and_inherited_roles_on_the_borrowed_transaction()
-> TestResult {
    let db = clone_db().await;
    let scope = platform_scope(&db).await;
    let groups: [GroupId; 4] = std::array::from_fn(|_| GroupId::new(Uuid::now_v7()));
    let [root, middle, leaf, separate] = groups;
    let person = UserId::new(Uuid::now_v7());
    let other = UserId::new(Uuid::now_v7());
    let unregistered = UserId::new(Uuid::now_v7());
    let mut tx = scope.begin().await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *tx)
        .await?;
    for group in groups {
        proxima::register_group(&mut tx, group).await?;
    }
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(person.into_inner())
        .execute(&mut *tx)
        .await?;
    for (group, member, relation) in [
        (root, middle.into(), Relation::Viewer),
        (middle, leaf.into(), Relation::Editor),
        (leaf, person.into(), Relation::Admin),
        (root, person.into(), Relation::Viewer),
        (root, person.into(), Relation::Ingest),
        (root, other.into(), Relation::Admin),
    ] {
        proxima::add_group_member(&mut tx, group, member, relation).await?;
    }

    // No authenticated owner context is needed, and none of these writes
    // has committed when the host borrows its existing transaction.
    let mut authority = scope
        .authority_on(
            &mut tx,
            AuthorityTargets {
                groups: vec![leaf, root, middle, separate, root],
                people: vec![person, unregistered, person],
                owners: vec![person.into()],
            },
        )
        .await?;
    let authority_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut **authority.transaction())
        .await?;
    assert_eq!(authority_pid, pid, "the caller's backend is retained");
    assert_eq!(
        authority.registered_owner(person.into()).await?,
        RegisteredOwner::Personal(person)
    );
    assert_eq!(
        authority.registered_owner(root.into()).await?,
        RegisteredOwner::Group(root)
    );
    assert!(matches!(
        authority.registered_owner(unregistered.into()).await,
        Err(StorageError::NotFound)
    ));
    {
        let mut group = authority.group(root)?;
        assert_eq!(group.id(), root);
        let actual: Vec<_> = group
            .direct_users(None)
            .await?
            .into_iter()
            .map(|row| (row.user, row.relation))
            .collect();
        let mut expected = vec![
            (person, Relation::Viewer),
            (person, Relation::Ingest),
            (other, Relation::Admin),
        ];
        expected.sort_by_key(|(user, _)| *user);
        assert_eq!(actual, expected);
        assert_eq!(group.direct_user_count(None).await?, 2);
        assert_eq!(group.direct_user_count(Some(Relation::Viewer)).await?, 1);
        let viewers = group.direct_users(Some(Relation::Viewer)).await?;
        assert_eq!(viewers.len(), 1);
        assert_eq!(
            (viewers[0].user, viewers[0].relation),
            (person, Relation::Viewer)
        );
        let children = group.direct_child_groups().await?;
        assert_eq!(children.len(), 1);
        assert_eq!(
            (children[0].group, children[0].relation),
            (middle, Relation::Viewer)
        );
        assert_eq!(
            group.direct_relations(middle.into()).await?,
            vec![Relation::Viewer]
        );
        assert!(group.direct_relations(leaf.into()).await?.is_empty());
        assert!(group.direct_relations(root.into()).await?.is_empty());
        assert_eq!(group.parent().await?, None);
    }
    {
        let mut group = authority.group(leaf)?;
        let parent = group.parent().await?.expect("attached leaf");
        assert_eq!((parent.group, parent.relation), (middle, Relation::Editor));
        let ancestry: Vec<_> = group
            .ancestors()
            .await?
            .into_iter()
            .map(|step| (step.group, step.relation))
            .collect();
        assert_eq!(
            ancestry,
            vec![(middle, Relation::Editor), (root, Relation::Viewer)]
        );
        assert_eq!(group.root().await?, root);
    }
    assert_eq!(
        authority.containment(root, root).await?,
        GroupContainment::SameGroup
    );
    assert_eq!(
        authority.containment(root, leaf).await?,
        GroupContainment::ContainsDescendant
    );
    assert_eq!(
        authority.containment(leaf, root).await?,
        GroupContainment::Unrelated
    );
    assert_eq!(
        authority.containment(root, separate).await?,
        GroupContainment::Unrelated
    );
    {
        let mut selected = authority.person(person).await?;
        assert_eq!(selected.id(), person);
        let direct: Vec<_> = selected
            .direct_groups()
            .await?
            .into_iter()
            .map(|row| (row.group, row.relation))
            .collect();
        let mut expected = vec![
            (root, Relation::Viewer),
            (root, Relation::Ingest),
            (leaf, Relation::Admin),
        ];
        expected.sort_by_key(|(group, _)| *group);
        assert_eq!(direct, expected);
        let roles = selected.effective_roles().await?;
        assert_eq!(roles.role_for(&person.into()), Some(Role::personal()));
        assert_eq!(roles.role_for(&leaf.into()), Some(Role::admin()));
        assert_eq!(roles.role_for(&middle.into()), Some(Role::editor()));
        assert_eq!(
            roles.role_for(&root.into()),
            Some(Role::viewer().join(Role::ingest()))
        );
        assert_eq!(roles.role_for(&separate.into()), None);
        assert_eq!(roles.role_for(&other.into()), None);
    }
    proxima::replace_group_member_relation(
        authority.transaction(),
        leaf,
        person.into(),
        Relation::Admin,
        Relation::Viewer,
    )
    .await?;
    let changed = authority.person(person).await?.effective_roles().await?;
    assert_eq!(changed.role_for(&leaf.into()), Some(Role::viewer()));
    assert_eq!(changed.role_for(&middle.into()), Some(Role::viewer()));
    assert!(
        authority
            .person(unregistered)
            .await?
            .direct_groups()
            .await?
            .is_empty()
    );
    drop(authority);
    tx.rollback().await?;
    assert_eq!(owner_and_membership_counts(&db, root, person).await, (0, 0));
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "person and subgroup replay outcomes share the host commit and rollback scenario"
)]
async fn authority_membership_outcomes_and_host_rows_share_commit_and_rollback() -> TestResult {
    let db = clone_db().await;
    let scope = platform_scope(&db).await;
    let group = GroupId::new(Uuid::now_v7());
    let child = GroupId::new(Uuid::now_v7());
    let person = UserId::new(Uuid::now_v7());
    let mut setup = scope.begin().await?;
    proxima::register_group(&mut setup, group).await?;
    proxima::register_group(&mut setup, child).await?;
    setup.commit().await?;
    for commit in [false, true] {
        let mut tx = scope.begin().await?;
        let mut authority = scope
            .authority_on(
                &mut tx,
                AuthorityTargets {
                    groups: vec![group, child],
                    people: vec![person],
                    ..AuthorityTargets::default()
                },
            )
            .await?;
        assert!(
            authority
                .group(group)?
                .direct_child_groups()
                .await?
                .is_empty()
        );
        for expected in [
            MembershipAddOutcome::Changed,
            MembershipAddOutcome::AlreadyPresent,
        ] {
            assert_eq!(
                proxima::add_group_member(
                    authority.transaction(),
                    group,
                    child.into(),
                    Relation::Editor,
                )
                .await?,
                expected
            );
        }
        let children = authority.group(group)?.direct_child_groups().await?;
        assert_eq!(children.len(), 1);
        assert_eq!(
            (children[0].group, children[0].relation),
            (child, Relation::Editor)
        );
        assert!(
            authority
                .person(person)
                .await?
                .direct_groups()
                .await?
                .is_empty()
        );
        for expected in [
            MembershipAddOutcome::Changed,
            MembershipAddOutcome::AlreadyPresent,
        ] {
            let outcome = proxima::add_group_member(
                authority.transaction(),
                group,
                person.into(),
                Relation::Viewer,
            )
            .await?;
            assert_eq!(outcome, expected);
            if outcome == MembershipAddOutcome::Changed {
                sqlx::query("INSERT INTO public.host_membership_transaction_rows (id) VALUES ($1)")
                    .bind(Uuid::now_v7())
                    .execute(&mut **authority.transaction())
                    .await?;
            }
        }
        let before_refusal: i64 =
            sqlx::query_scalar("SELECT count(*) FROM public.host_membership_transaction_rows")
                .fetch_one(&mut **authority.transaction())
                .await?;
        let refused = proxima::add_group_member(
            authority.transaction(),
            group,
            child.into(),
            Relation::Viewer,
        )
        .await;
        if matches!(&refused, Ok(MembershipAddOutcome::Changed)) {
            sqlx::query("INSERT INTO public.host_membership_transaction_rows (id) VALUES ($1)")
                .bind(Uuid::now_v7())
                .execute(&mut **authority.transaction())
                .await?;
        }
        assert!(matches!(refused, Err(StorageError::Conflict(_))));
        assert_eq!(
            authority.group(group)?.direct_child_groups().await?,
            children
        );
        let after_refusal: i64 =
            sqlx::query_scalar("SELECT count(*) FROM public.host_membership_transaction_rows")
                .fetch_one(&mut **authority.transaction())
                .await?;
        assert_eq!(after_refusal, before_refusal);
        assert_eq!(authority.group(group)?.direct_user_count(None).await?, 1);
        assert_eq!(
            authority
                .person(person)
                .await?
                .effective_roles()
                .await?
                .role_for(&group.into()),
            Some(Role::viewer())
        );
        drop(authority);
        if commit {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        let expected = i64::from(commit);
        assert_eq!(host_row_count(&db).await, expected);
        assert_eq!(
            owner_and_membership_counts(&db, group, person).await,
            (1, expected)
        );
    }
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one validated scope is tested against incompatible transactions and selections"
)]
async fn authority_refuses_foreign_transactions_fixed_snapshots_and_undeclared_targets()
-> TestResult {
    let db = clone_db().await;
    let foreign = clone_db().await;
    let scope = platform_scope(&db).await;
    let runtime = PgPool::connect(db.runtime_url()).await?;
    let foreign_pool = PgPool::connect(foreign.platform_url()).await?;
    for pool in [&runtime, &foreign_pool] {
        let mut tx = pool.begin().await?;
        assert!(matches!(
            scope
                .authority_on(&mut tx, AuthorityTargets::default())
                .await,
            Err(StorageError::ConstraintViolation(_))
        ));
        tx.rollback().await?;
    }
    let platform = PgPool::connect(db.platform_url()).await?;
    for serializable in [false, true] {
        let mut tx = platform.begin().await?;
        let query = if serializable {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        } else {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        };
        query.execute(&mut *tx).await?;
        assert!(matches!(
            scope
                .authority_on(&mut tx, AuthorityTargets::default())
                .await,
            Err(StorageError::MembershipIsolation)
        ));
        tx.rollback().await?;
    }
    let unknown = Uuid::now_v7();
    for targets in [
        AuthorityTargets {
            groups: vec![GroupId::new(unknown)],
            ..AuthorityTargets::default()
        },
        AuthorityTargets {
            owners: vec![OwnerId::new(unknown)],
            ..AuthorityTargets::default()
        },
    ] {
        let mut tx = platform.begin().await?;
        assert!(matches!(
            scope.authority_on(&mut tx, targets).await,
            Err(StorageError::NotFound)
        ));
        tx.rollback().await?;
    }
    let registered_group = GroupId::new(Uuid::now_v7());
    let registered_person = UserId::new(Uuid::now_v7());
    let mut setup = scope.begin().await?;
    proxima::register_group(&mut setup, registered_group).await?;
    sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(registered_person.into_inner())
        .execute(&mut *setup)
        .await?;
    setup.commit().await?;
    for targets in [
        AuthorityTargets {
            groups: vec![GroupId::new(registered_person.into_inner())],
            ..AuthorityTargets::default()
        },
        AuthorityTargets {
            people: vec![UserId::new(registered_group.into_inner())],
            ..AuthorityTargets::default()
        },
    ] {
        let mut tx = platform.begin().await?;
        assert!(matches!(
            scope.authority_on(&mut tx, targets).await,
            Err(StorageError::ConstraintViolation(_))
        ));
        tx.rollback().await?;
    }
    let person = UserId::new(Uuid::now_v7());
    let mut tx = platform.begin().await?;
    let mut authority = scope
        .authority_on(
            &mut tx,
            AuthorityTargets {
                people: vec![person],
                ..AuthorityTargets::default()
            },
        )
        .await?;
    assert!(matches!(
        authority.group(GroupId::new(Uuid::now_v7())),
        Err(StorageError::ConstraintViolation(_))
    ));
    assert!(matches!(
        authority.person(UserId::new(Uuid::now_v7())).await,
        Err(StorageError::ConstraintViolation(_))
    ));
    assert!(matches!(
        authority
            .registered_owner(OwnerId::new(Uuid::now_v7()))
            .await,
        Err(StorageError::ConstraintViolation(_))
    ));
    assert!(matches!(
        authority
            .containment(GroupId::new(Uuid::now_v7()), GroupId::new(Uuid::now_v7()))
            .await,
        Err(StorageError::ConstraintViolation(_))
    ));
    assert!(matches!(
        authority.registered_owner(person.into()).await,
        Err(StorageError::NotFound)
    ));
    assert_eq!(
        authority
            .person(person)
            .await?
            .effective_roles()
            .await?
            .role_for(&person.into()),
        Some(Role::personal())
    );
    proxima::register_group(authority.transaction(), GroupId::new(person.into_inner())).await?;
    assert!(matches!(
        authority.person(person).await,
        Err(StorageError::ConstraintViolation(_))
    ));
    drop(authority);
    tx.rollback().await?;
    runtime.close().await;
    foreign_pool.close().await;
    platform.close().await;
    Ok(())
}

#[tokio::test]
async fn authority_drop_keeps_removal_and_first_add_fenced_until_host_commit() -> TestResult {
    let db = clone_db().await;
    let scope = platform_scope(&db).await;
    let probe = PgPool::connect(db.platform_url()).await?;
    let group = GroupId::new(Uuid::now_v7());
    let other_group = GroupId::new(Uuid::now_v7());
    let existing = UserId::new(Uuid::now_v7());
    let newcomer = UserId::new(Uuid::now_v7());
    let mut setup = scope.begin().await?;
    proxima::register_group(&mut setup, group).await?;
    proxima::register_group(&mut setup, other_group).await?;
    proxima::add_group_member(&mut setup, group, existing.into(), Relation::Viewer).await?;
    setup.commit().await?;
    for (person, remove) in [(existing, true), (newcomer, false)] {
        let mut tx = scope.begin().await?;
        let mut authority = scope
            .authority_on(
                &mut tx,
                AuthorityTargets {
                    people: vec![person],
                    ..AuthorityTargets::default()
                },
            )
            .await?;
        assert_eq!(
            authority
                .person(person)
                .await?
                .direct_groups()
                .await?
                .is_empty(),
            !remove
        );
        drop(authority);
        let mut writer = scope.begin().await?;
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *writer)
            .await?;
        let pending = tokio::spawn(async move {
            if remove {
                proxima::remove_group_member(&mut writer, group, person.into()).await?;
            } else {
                proxima::add_group_member(&mut writer, other_group, person.into(), Relation::Admin)
                    .await?;
            }
            writer.commit().await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        });
        wait_for_authority_lock(&probe, pid).await?;
        assert!(
            !pending.is_finished(),
            "the outer transaction still holds the fence"
        );
        // A third tree and another person can commit while this writer waits.
        let unrelated = GroupId::new(Uuid::now_v7());
        let mut independent = scope.begin().await?;
        tokio::time::timeout(Duration::from_secs(15), async {
            proxima::register_group(&mut independent, unrelated).await?;
            proxima::add_group_member(
                &mut independent,
                unrelated,
                UserId::new(Uuid::now_v7()).into(),
                Relation::Viewer,
            )
            .await?;
            independent.commit().await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        })
        .await??;
        sqlx::query("INSERT INTO public.host_membership_transaction_rows (id) VALUES ($1)")
            .bind(Uuid::now_v7())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        pending.await??;
        let mut verify = scope.begin().await?;
        let roles = proxima::roles_for_subject(&mut verify, person).await?;
        assert_eq!(roles.role_for(&group.into()), None);
        assert_eq!(
            roles.role_for(&other_group.into()),
            (!remove).then_some(Role::admin())
        );
        verify.rollback().await?;
    }
    assert_eq!(host_row_count(&db).await, 2);
    probe.close().await;
    Ok(())
}

#[tokio::test]
async fn authority_waits_for_a_relation_replacement_then_reads_its_committed_role() -> TestResult {
    let db = clone_db().await;
    let scope = platform_scope(&db).await;
    let probe = PgPool::connect(db.platform_url()).await?;
    let group = GroupId::new(Uuid::now_v7());
    let person = UserId::new(Uuid::now_v7());
    let mut setup = scope.begin().await?;
    proxima::register_group(&mut setup, group).await?;
    proxima::add_group_member(&mut setup, group, person.into(), Relation::Admin).await?;
    setup.commit().await?;
    let mut writer = scope.begin().await?;
    proxima::replace_group_member_relation(
        &mut writer,
        group,
        person.into(),
        Relation::Admin,
        Relation::Viewer,
    )
    .await?;
    let mut reader = scope.begin().await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *reader)
        .await?;
    let reader_scope = scope.clone();
    let pending = tokio::spawn(async move {
        let mut authority = reader_scope
            .authority_on(
                &mut reader,
                AuthorityTargets {
                    groups: vec![group],
                    people: vec![person],
                    ..AuthorityTargets::default()
                },
            )
            .await?;
        let roles = authority.person(person).await?.effective_roles().await?;
        let relations = authority
            .group(group)?
            .direct_relations(person.into())
            .await?;
        sqlx::query("INSERT INTO public.host_membership_transaction_rows (id) VALUES ($1)")
            .bind(Uuid::now_v7())
            .execute(&mut **authority.transaction())
            .await?;
        drop(authority);
        reader.commit().await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((roles, relations))
    });
    wait_for_authority_lock(&probe, pid).await?;
    assert!(!pending.is_finished());
    writer.commit().await?;
    let (roles, relations) = pending.await??;
    assert_eq!(roles.role_for(&group.into()), Some(Role::viewer()));
    assert_eq!(relations, vec![Relation::Viewer]);
    assert_eq!(host_row_count(&db).await, 1);
    probe.close().await;
    Ok(())
}
