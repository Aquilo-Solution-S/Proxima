//! The carry rule (`goal_text_carried`) runs as the writer, under owner RLS, and
//! still sees the predecessor of the series: a scope that may write an owner's
//! Goals sees them through the write policy even when it may not read them.

use proxima_core::{Credentials, GroupId, OwnerRef, OwnerRoles, Role, UserId};
use proxima_storage_pg::begin_owner_transaction;
use sqlx::PgConnection;
use uuid::Uuid;

/// A version of the series, written the way a runtime scope writes one.
async fn insert_version(
    connection: &mut PgConnection,
    handle: Uuid,
    owner: Uuid,
    state: &str,
    text: Option<&str>,
    request_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.goal (handle, t, owner_id, title, text, state, request_id, close_fact_t)
         VALUES ($1, uuidv7(), $2, 'carry', $3, $4::proxima_core.goal_state, $5, NULL)",
    )
    .bind(handle)
    .bind(owner)
    .bind(text)
    .bind(state)
    .bind(request_id)
    .execute(connection)
    .await
    .map(|_| ())
}

async fn assert_carry_refusal(
    connection: &mut PgConnection,
    what: &str,
    outcome: Result<(), sqlx::Error>,
) {
    let error = outcome.expect_err(what);
    let database = error.as_database_error().expect("database refusal");
    assert_eq!(database.code().as_deref(), Some("23514"), "{what}: {error}");
    assert!(
        database.message().contains("goal_text_carried"),
        "{what}: {error}"
    );
    sqlx::query("ROLLBACK TO SAVEPOINT carry")
        .execute(connection)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_carry_rule_binds_a_runtime_scope_that_cannot_read_the_series() {
    let (database, admin, runtime, platform, platform_role, runtime_role, _password) =
        super::setup_full_schema().await;
    let owner = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.owners(owner_id, kind) VALUES ($1, 'group')")
        .bind(owner)
        .execute(&admin)
        .await
        .unwrap();
    let handle = Uuid::now_v7();
    let mut seed = admin.acquire().await.unwrap();
    sqlx::query(
        "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, 'core/task-goal-v1', $2, uuidv7())",
    )
    .bind(handle)
    .bind(owner)
    .execute(&mut *seed)
    .await
    .unwrap();
    insert_version(
        &mut seed,
        handle,
        owner,
        "Active",
        Some("kept"),
        "carry-seed",
    )
    .await
    .unwrap();
    drop(seed);

    let verifier = super::SyntheticVerifier {
        roles: OwnerRoles::for_subject(
            UserId::new(Uuid::now_v7()),
            [(OwnerRef::Group(GroupId::new(owner)), Role::admin())],
        )
        .unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("scope".into()))
        .await
        .unwrap();

    // Once as a scope that reads and writes the owner's Goals, once with the
    // read set emptied: the predecessor must be seen either way.
    for (index, read_goals) in [true, false].into_iter().enumerate() {
        let mut tx = begin_owner_transaction(&runtime, authz.owner_scope().unwrap())
            .await
            .unwrap();
        if !read_goals {
            sqlx::query("SELECT set_config('app.read_goal', '{}', true)")
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        let request = |what: &str| format!("carry-{index}-{what}");
        sqlx::query("SAVEPOINT carry")
            .execute(&mut *tx)
            .await
            .unwrap();
        let dropped =
            insert_version(&mut tx, handle, owner, "Paused", None, &request("null")).await;
        assert_carry_refusal(&mut tx, "a NULL text on a state change", dropped).await;
        let changed = insert_version(
            &mut tx,
            handle,
            owner,
            "Paused",
            Some("other"),
            &request("other"),
        )
        .await;
        assert_carry_refusal(&mut tx, "another text on a state change", changed).await;
        insert_version(
            &mut tx,
            handle,
            owner,
            "Active",
            Some("modified"),
            &request("modify"),
        )
        .await
        .expect("a modify may change the text");
        tx.rollback().await.unwrap();
    }

    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
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
    insert_version(
        &mut seed,
        handle,
        holder,
        "Active",
        Some("kept"),
        "join-seed",
    )
    .await
    .unwrap();
    drop(seed);

    // A scope that writes only the other owner's Goals sees neither the head
    // nor the versions of the series. Under its own owner id it may write the
    // row, so the trigger is what has to refuse it: whatever the state and
    // text, the series is not its owner's.
    let verifier = super::SyntheticVerifier {
        roles: OwnerRoles::for_subject(
            UserId::new(Uuid::now_v7()),
            [(OwnerRef::Group(GroupId::new(other)), Role::admin())],
        )
        .unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("scope".into()))
        .await
        .unwrap();
    let mut tx = begin_owner_transaction(&runtime, authz.owner_scope().unwrap())
        .await
        .unwrap();
    sqlx::query("SAVEPOINT carry")
        .execute(&mut *tx)
        .await
        .unwrap();
    for (state, text) in [
        ("Paused", Some("other")),
        ("Active", Some("other")),
        ("Paused", None),
    ] {
        let joined = insert_version(
            &mut tx,
            handle,
            other,
            state,
            text,
            &format!("join-{state}-{text:?}"),
        )
        .await;
        assert_carry_refusal(&mut tx, "another owner joins the series", joined).await;
    }
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
