//! The role probes a host runs on its own connection.
//!
//! `roles_for_subject` and `group_role` are the queries `PgOwnerAccessResolver`
//! runs, taking the caller's connection instead of opening a transaction. What
//! is pinned here: they answer exactly what the resolver answers for every
//! membership shape (including several relations in one group, which resolve
//! to their join), they see rows written earlier in the caller's open
//! transaction, and they leave that transaction open and usable.

use proxima_core::{
    AccessError, GroupId, OwnerAccessPort, OwnerRef, OwnerRoles, Relation, Role, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::{PgOwnerAccessResolver, PgStorage, group_role, roles_for_subject};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

async fn seed_membership<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    group: GroupId,
    member: UserId,
    relation: Relation,
) {
    sqlx::query(
        "INSERT INTO proxima_core.group_memberships (group_id, member_user_id, relation)
         VALUES ($1, $2, $3)",
    )
    .bind(group.into_inner())
    .bind(member.into_inner())
    .bind(relation)
    .execute(executor)
    .await
    .expect("seed membership");
}

fn user() -> UserId {
    UserId::new(Uuid::now_v7())
}

fn group() -> GroupId {
    GroupId::new(Uuid::now_v7())
}

/// A migrated database and a pool onto it; the caller drops the database.
async fn fixture() -> (String, sqlx::PgPool) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(error) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {error}");
    }
    let pg = PgStorage::connect(&db_url(&db_name))
        .await
        .expect("connect");
    pg.run_before_owner_rls_migrations().await.expect("migrate");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&db_url(&db_name))
        .await
        .expect("pool");
    (db_name, pool)
}

#[tokio::test]
async fn probes_answer_what_the_resolver_answers_for_every_membership_shape() {
    let (db_name, pool) = fixture().await;
    let result = async {
        let subject = user();
        let stranger = user();
        let loner = user();
        let single = group();
        let joined = group();
        let promoted = group();
        let strangers_only = group();
        // One relation.
        seed_membership(&pool, single, subject, Relation::Editor).await;
        // Two relations in one group: their join, never the last row.
        seed_membership(&pool, joined, subject, Relation::Viewer).await;
        seed_membership(&pool, joined, subject, Relation::Ingest).await;
        seed_membership(&pool, promoted, subject, Relation::Viewer).await;
        seed_membership(&pool, promoted, subject, Relation::Admin).await;
        // Someone else's membership must not leak into the subject's answer.
        seed_membership(&pool, strangers_only, stranger, Relation::Admin).await;
        seed_membership(&pool, single, stranger, Relation::Viewer).await;

        let resolver = PgOwnerAccessResolver::new(pool.clone());
        let mut conn = pool.acquire().await?;
        let groups = [single, joined, promoted, strangers_only];

        // `loner` holds nothing: no membership at all.
        for who in [subject, stranger, loner] {
            let eager = resolver.resolve_roles_for_subject(who).await?;
            let probed = roles_for_subject(&mut conn, who).await?;
            assert_eq!(probed, eager, "roles_for_subject must equal the resolver");
            for group in groups {
                assert_eq!(
                    group_role(&mut conn, who, group).await?,
                    resolver.resolve_group_role(who, group).await?,
                    "group_role must equal the resolver for {who:?} in {group:?}"
                );
                assert_eq!(
                    group_role(&mut conn, who, group).await?,
                    probed.role_for(&OwnerRef::Group(group)),
                    "group_role must equal the same subject's map entry"
                );
            }
        }

        // The values themselves, so equality cannot be two wrong answers.
        let roles = roles_for_subject(&mut conn, subject).await?;
        assert_eq!(
            roles.role_for(&OwnerRef::Group(single)),
            Some(Role::editor())
        );
        assert_eq!(
            roles.role_for(&OwnerRef::Group(joined)),
            Some(Role::viewer().join(Role::ingest()))
        );
        assert_eq!(
            roles.role_for(&OwnerRef::Group(promoted)),
            Some(Role::admin())
        );
        assert_eq!(roles.role_for(&OwnerRef::Group(strangers_only)), None);
        assert_eq!(
            group_role(&mut conn, subject, joined).await?,
            Some(Role::viewer().join(Role::ingest()))
        );
        assert_eq!(group_role(&mut conn, subject, strangers_only).await?, None);
        assert_eq!(
            roles_for_subject(&mut conn, loner).await?,
            OwnerRoles::empty_for_subject(loner),
            "no membership is the subject's own Personal role and nothing else"
        );

        // A Personal owner is never an answer beyond the subject's own.
        assert_eq!(
            roles.role_for(&OwnerRef::Personal(subject)),
            Some(Role::personal())
        );
        assert_eq!(roles.role_for(&OwnerRef::Personal(stranger)), None);
        assert_eq!(roles.role_for(&OwnerRef::Personal(loner)), None);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("probes match the resolver");
}

#[tokio::test]
async fn a_probe_sees_the_callers_open_transaction_and_leaves_it_usable() {
    let (db_name, pool) = fixture().await;
    let result = async {
        let subject = user();
        let team = group();
        let resolver = PgOwnerAccessResolver::new(pool.clone());

        let mut tx = pool.begin().await?;
        assert_eq!(group_role(&mut tx, subject, team).await?, None);
        seed_membership(&mut *tx, team, subject, Relation::Viewer).await;

        // The row exists only inside the caller's transaction, and the probe
        // run on it sees the row; another connection does not.
        assert_eq!(
            group_role(&mut tx, subject, team).await?,
            Some(Role::viewer())
        );
        assert_eq!(
            roles_for_subject(&mut tx, subject)
                .await?
                .role_for(&OwnerRef::Group(team)),
            Some(Role::viewer())
        );
        assert_eq!(resolver.resolve_group_role(subject, team).await?, None);

        // The probe neither committed nor rolled back: the transaction takes
        // further writes and the next probe sees them.
        seed_membership(&mut *tx, team, subject, Relation::Admin).await;
        assert_eq!(
            group_role(&mut tx, subject, team).await?,
            Some(Role::admin())
        );
        assert_eq!(resolver.resolve_group_role(subject, team).await?, None);

        tx.commit().await?;
        assert_eq!(
            resolver.resolve_group_role(subject, team).await?,
            Some(Role::admin()),
            "the caller's commit is what publishes the rows"
        );

        // And a rollback by the caller takes the probed rows with it.
        let mut tx = pool.begin().await?;
        let other = group();
        seed_membership(&mut *tx, other, subject, Relation::Editor).await;
        assert_eq!(
            group_role(&mut tx, subject, other).await?,
            Some(Role::editor())
        );
        tx.rollback().await?;
        assert_eq!(resolver.resolve_group_role(subject, other).await?, None);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("probes run inside the caller's transaction");
}

#[tokio::test]
async fn a_failing_connection_is_a_resolution_error_not_an_empty_answer() {
    let (db_name, pool) = fixture().await;
    let result = async {
        let subject = user();
        let mut tx = pool.begin().await?;
        // Abort the transaction: every further statement on it fails.
        sqlx::query("SELECT 1/0")
            .execute(&mut *tx)
            .await
            .expect_err("division by zero aborts the transaction");

        let roles = roles_for_subject(&mut tx, subject).await;
        assert!(
            matches!(roles, Err(AccessError::Resolution(_))),
            "an unreadable store must not read as no membership: {roles:?}"
        );
        let role = group_role(&mut tx, subject, group()).await;
        assert!(
            matches!(role, Err(AccessError::Resolution(_))),
            "an unreadable store must not read as no role: {role:?}"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("probe failures surface");
}
