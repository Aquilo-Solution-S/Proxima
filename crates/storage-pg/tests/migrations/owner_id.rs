//! Migration 0027 (`_v033_`) makes an owner one owner id: the kind lives only
//! in `proxima_core.owners`, and a membership's group must be registered as a
//! group.
//!
//! The fixture is a database standing at the released 0026 schema (v0.0.29),
//! with rows under both kinds, written the way the released binary wrote
//! them. The upgrade is then the real runner, ledger included.
use std::borrow::Cow;

use proxima_pg_testkit::{create_db, db_url, drop_db, split_role_urls};
use proxima_storage_pg::{begin_migration_transaction, core_migrator};
use uuid::Uuid;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A database with every migration up to 0026 applied and recorded.
async fn database_at_0026(db_name: &str) -> TestResult<super::MigrationFixture> {
    create_db(db_name).await?;
    let (_, platform_url) = split_role_urls(db_name).await?;
    let platform = sqlx::PgPool::connect(&platform_url).await?;
    let mut staged = core_migrator();
    staged.migrations = Cow::Owned(
        staged
            .iter()
            .filter(|migration| migration.version < 27)
            .cloned()
            .collect(),
    );
    let mut connection = platform.acquire().await?;
    let mut migration = begin_migration_transaction(&mut connection).await?;
    staged.run(migration.as_mut()).await?;
    migration.commit().await?;
    drop(connection);
    platform.close().await;
    let pg = super::connect_historical_fixture(db_name).await?;
    assert!(
        super::column_exists(&pg, "source_cursors", "owner_kind").await,
        "the fixture stands at 0026, where cursors still carry the kind"
    );
    Ok(pg)
}

/// The v0.0.29 shapes, one row per kind in every table that stored a kind.
async fn seed_both_kinds(pool: &sqlx::PgPool, user: Uuid, group: Uuid) -> TestResult {
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal'), ($2, 'group')",
    )
    .bind(user)
    .bind(group)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.group_memberships (group_id, member_user_id, relation)
         VALUES ($1, $2, 'admin')",
    )
    .bind(group)
    .bind(user)
    .execute(pool)
    .await?;
    for (kind, id) in [("personal", user), ("group", group)] {
        sqlx::query(
            "INSERT INTO proxima_core.source_cursors (owner_kind, owner_id, source, cursor)
             VALUES ($1::proxima_core.owner_kind, $2, 'src', $3)",
        )
        .bind(kind)
        .bind(id)
        .bind(kind.as_bytes())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.delegated_authority_grants
                 (delegation_id, subject_user_id, owner_kind, owner_id, tool_name,
                  read_ceiling, write_ceiling, expires_at, auth_epoch, issued_at)
             VALUES (uuidv7(), $1, $2::proxima_core.owner_kind, $3, 'core_remember',
                     'fact', 'fact', TIMESTAMPTZ '2030-01-01 00:00:00Z', 1, now())",
        )
        .bind(user)
        .bind(kind)
        .bind(id)
        .execute(pool)
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn a_v0_0_29_database_with_both_kinds_keeps_every_row_readable() -> TestResult {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        let pg = database_at_0026(&db_name).await?;
        let pool = pg.pool_for_tests();
        let (user, group) = (Uuid::now_v7(), Uuid::now_v7());
        seed_both_kinds(pool, user, group).await?;

        super::apply_current_migrations(&pg).await?;

        for (id, cursor) in [(user, b"personal".to_vec()), (group, b"group".to_vec())] {
            let read: Vec<u8> = sqlx::query_scalar(
                "SELECT cursor FROM proxima_core.source_cursors
                  WHERE owner_id = $1 AND source = 'src'",
            )
            .bind(id)
            .fetch_one(pool)
            .await?;
            assert_eq!(read, cursor, "the cursor of {id} reads by its bare id");
            let grants: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM proxima_core.delegated_authority_grants
                  WHERE owner_id = $1",
            )
            .bind(id)
            .fetch_one(pool)
            .await?;
            assert_eq!(grants, 1, "the grant on {id} reads by its bare id");
        }
        let kinds: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT owner_id, kind::text FROM proxima_core.owners
              WHERE owner_id = ANY($1) ORDER BY kind",
        )
        .bind(vec![user, group])
        .fetch_all(pool)
        .await?;
        assert_eq!(
            kinds,
            vec![(group, "group".to_owned()), (user, "personal".to_owned())],
            "the registry keeps both kinds"
        );
        let members: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM proxima_core.group_memberships WHERE group_id = $1",
        )
        .bind(group)
        .fetch_one(pool)
        .await?;
        assert_eq!(members, 1, "the membership survives");
        for (table, column) in [
            ("source_cursors", "owner_kind"),
            ("delegated_authority_grants", "owner_kind"),
            ("publication_origin", "original_owner_kind"),
        ] {
            assert!(
                !super::column_exists(&pg, table, column).await,
                "{table}.{column} is gone"
            );
        }

        // The upgrade is idempotent: running it again changes nothing.
        super::apply_core_migrations(pool, 27..=27).await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

/// A membership names a group: the database refuses one whose `group_id`
/// is registered as a user, and one whose `group_id` is not registered.
#[tokio::test]
async fn the_database_refuses_a_membership_under_a_users_id() -> TestResult {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    let result: TestResult = async {
        create_db(&db_name).await?;
        let pg = proxima_storage_pg::PgStorage::connect_for_tests(db_url(&db_name)).await?;
        super::apply_current_migrations(&pg).await?;
        let pool = pg.pool_for_tests();
        let (user, member) = (Uuid::now_v7(), Uuid::now_v7());
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(user)
            .execute(pool)
            .await?;
        for group_id in [user, Uuid::now_v7()] {
            let err = sqlx::query(
                "INSERT INTO proxima_core.group_memberships (group_id, member_user_id, relation)
                 VALUES ($1, $2, 'viewer')",
            )
            .bind(group_id)
            .bind(member)
            .execute(pool)
            .await
            .expect_err("a membership's group must be a registered group");
            assert!(
                err.to_string()
                    .contains("group_memberships_group_registered_fk"),
                "{err}"
            );
        }
        let err = sqlx::query(
            "INSERT INTO proxima_core.group_memberships
                 (group_id, member_user_id, relation, group_kind)
             VALUES ($1, $2, 'viewer', 'personal')",
        )
        .bind(user)
        .bind(member)
        .execute(pool)
        .await
        .expect_err("the kind column cannot be spelled around");
        assert!(
            err.to_string().contains("group_memberships_group_kind_chk"),
            "{err}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}
