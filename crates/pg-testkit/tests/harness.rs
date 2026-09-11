use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

use proxima_pg_testkit::{
    DbGuard, admin_url, create_db, db_url, drop_db, drop_stale_templates, ensure_template,
    sweep_stale_test_dbs, unique_db_name,
};

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
async fn force_drop_terminates_a_live_backend() {
    let name = unique_db_name("proxima_test");
    create_db(&name).await.expect("create");
    let pool = PgPool::connect(&db_url(&name)).await.expect("pool");
    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .expect("use the clone");
    drop_db(&name).await.expect("FORCE drop");
    assert!(!exists(&name).await, "clone must be gone");
    pool.close().await;
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
async fn sweep_drops_a_backdated_tracking_row() {
    let name = unique_db_name("proxima_test");
    create_db(&name).await.expect("create");
    let mut conn = PgConnection::connect(&admin_url())
        .await
        .expect("admin connect");
    sqlx::query(
        "UPDATE _proxima_test.databases
         SET created_at = now() - interval '1 hour'
         WHERE db_name = $1",
    )
    .bind(&name)
    .execute(&mut conn)
    .await
    .expect("backdate");
    conn.close().await.expect("close");

    let dropped = sweep_stale_test_dbs().await.expect("sweep");
    assert!(dropped >= 1, "sweep must drop the backdated clone");
    assert!(!exists(&name).await, "backdated clone must be gone");
}

#[tokio::test]
async fn uuid_v7_now_is_not_swept_as_untracked_grace() {
    let name = format!("proxima_test_{}", Uuid::now_v7().simple());
    create_db(&name).await.expect("create");
    // Fresh clones are newer than process_start - 5m, so the untracked
    // prefix path must not delete them. The tracking row is also newer
    // than process_start, so the tracked path must not either.
    let dropped = sweep_stale_test_dbs().await.expect("sweep");
    let _ = dropped;
    assert!(exists(&name).await, "live clone must survive a later sweep");
    drop_db(&name).await.expect("cleanup");
}

#[tokio::test]
async fn ensure_template_drops_other_hashes_in_the_family() {
    let keep = "proxima_tmpl_core_aaaaaaaaaaaaaaaa";
    let stale = "proxima_tmpl_core_bbbbbbbbbbbbbbbb";
    let other_family = "proxima_tmpl_code_cccccccccccccccc";
    create_db(keep).await.expect("keep");
    create_db(stale).await.expect("stale");
    create_db(other_family).await.expect("other family");

    ensure_template(keep, |_| async { Ok(()) })
        .await
        .expect("reuse keep");

    assert!(exists(keep).await, "current hash must remain");
    assert!(!exists(stale).await, "sibling core hash must be dropped");
    assert!(
        exists(other_family).await,
        "code templates are a different family"
    );

    drop_db(keep).await.expect("cleanup keep");
    drop_db(other_family).await.expect("cleanup other");
    let _ = drop_stale_templates(keep).await;
}
