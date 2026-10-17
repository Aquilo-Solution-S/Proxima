//! A series belongs to its head's owner: `goal_owner_matches_head_fk` refuses a
//! version of another owner even from a scope that may write that owner's Goals.

use proxima_core::{Credentials, GroupId, OwnerId, OwnerRoles, Role, UserId};
use proxima_storage_pg::begin_owner_transaction;
use sqlx::PgConnection;
use uuid::Uuid;

/// A version of the series, written the way a runtime scope writes one.
async fn insert_version(
    connection: &mut PgConnection,
    handle: Uuid,
    owner: Uuid,
    request_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.goal (handle, t, owner_id, title, text, state, request_id, close_fact_t, \"by\", via)
         VALUES ($1, uuidv7(), $2, 'join', 'text', 'Active', $3, NULL, '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
    )
    .bind(handle)
    .bind(owner)
    .bind(request_id)
    .execute(connection)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn a_scope_of_another_owner_cannot_join_the_series() {
    let (database, admin, runtime, platform, platform_role, runtime_role, _password) =
        super::setup_full_schema().await;
    let (holder, other) = (Uuid::now_v7(), Uuid::now_v7());
    for owner in [holder, other] {
        sqlx::query("INSERT INTO proxima_core.owners(owner_id, kind) VALUES ($1, 'group')")
            .bind(owner)
            .execute(&admin)
            .await
            .unwrap();
    }
    let handle = Uuid::now_v7();
    let mut seed = admin.acquire().await.unwrap();
    sqlx::query(
        "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, 'core/task-goal-v1', $2, uuidv7())",
    )
    .bind(handle)
    .bind(holder)
    .execute(&mut *seed)
    .await
    .unwrap();
    insert_version(&mut seed, handle, holder, "join-seed")
        .await
        .unwrap();
    drop(seed);

    // A scope that writes only the other owner's Goals sees neither the head
    // nor the versions of the series, and under its own owner id it may write
    // the row. The foreign key is what refuses it: the series is not its
    // owner's.
    let verifier = super::SyntheticVerifier {
        roles: OwnerRoles::for_subject(
            UserId::new(Uuid::now_v7()),
            [(OwnerId::from(GroupId::new(other)), Role::admin())],
        )
        .unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("scope".into()))
        .await
        .unwrap();
    let mut tx = begin_owner_transaction(&runtime, authz.owner_scope().unwrap())
        .await
        .unwrap();
    let joined = insert_version(&mut tx, handle, other, "join-other").await;
    let error = joined.expect_err("another owner joins the series");
    let database_error = error.as_database_error().expect("database refusal");
    assert_eq!(database_error.code().as_deref(), Some("23503"), "{error}");
    assert_eq!(
        database_error.constraint(),
        Some("goal_owner_matches_head_fk"),
        "{error}"
    );
    tx.rollback().await.unwrap();

    let versions: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.goal WHERE handle = $1")
            .bind(handle)
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(versions, 1, "the series keeps only its own owner's version");

    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
}
