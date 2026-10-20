use sqlx::{Connection, PgConnection};

use proxima_pg_testkit::{DbGuard, admin_url, create_db, drop_db, unique_db_name};

async fn exists(name: &str) -> bool {
    let mut conn = PgConnection::connect(&admin_url())
        .await
        .expect("admin connect");
    let found = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
    )
    .bind(name)
    .fetch_one(&mut conn)
    .await
    .expect("exists");
    conn.close().await.expect("close");
    found
}

#[tokio::test]
async fn passing_guard_drops_the_database() {
    let name = unique_db_name("proxima_test");
    create_db(&name).await.expect("create");
    {
        let _guard = DbGuard::adopt(name.clone());
    }
    assert!(!exists(&name).await, "passing guard must drop");
}

#[tokio::test]
async fn panicking_guard_keeps_the_database() {
    let name = unique_db_name("proxima_test");
    create_db(&name).await.expect("create");
    let join = std::thread::spawn({
        let name = name.clone();
        move || {
            let _guard = DbGuard::adopt(name);
            panic!("intentional");
        }
    });
    assert!(join.join().is_err(), "thread must panic");
    assert!(exists(&name).await, "panic must keep the clone");
    drop_db(&name).await.expect("cleanup kept clone");
}

#[tokio::test]
async fn the_connection_budget_reads_the_servers_slots_and_their_use() {
    let budget = proxima_pg_testkit::connection_budget()
        .await
        .expect("budget");
    assert!(budget.max_connections > budget.superuser_reserved);
    assert!(
        budget.client_backends >= 1,
        "the budget's own connection is a client backend: {budget}"
    );
    assert!(budget.non_superuser_backends <= budget.client_backends);

    let exhausted = proxima_pg_testkit::describe_connection_error(
        "runtime connection",
        &"pool timed out while waiting for an open connection",
    )
    .await;
    assert!(
        exhausted.starts_with(
            "runtime connection: pool timed out while waiting for an open connection; test server: max_connections="
        ),
        "{exhausted}"
    );
    let other =
        proxima_pg_testkit::describe_connection_error("platform validation", &"scope refused")
            .await;
    assert_eq!(other, "platform validation: scope refused");
}
