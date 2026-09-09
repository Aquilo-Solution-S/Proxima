use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use proxima_pg_testkit::{drop_db, ensure_template, unique_db_name};

#[tokio::test]
async fn failed_template_build_is_not_reused() {
    let template = unique_db_name("proxima_failed_template");
    let failed = ensure_template(&template, |pool| async move {
        sqlx::query("CREATE TABLE migration_marker (id integer)")
            .execute(&pool)
            .await?;
        Err(sqlx::Error::Protocol("simulated migration failure".into()))
    })
    .await;

    let rebuilt = Arc::new(AtomicBool::new(false));
    let built = Arc::clone(&rebuilt);
    let retry = ensure_template(&template, |pool| async move {
        built.store(true, Ordering::SeqCst);
        // The failed attempt must not leave a partially migrated template.
        sqlx::query("CREATE TABLE migration_marker (id integer)")
            .execute(&pool)
            .await?;
        Ok(())
    })
    .await;

    drop_db(&template).await.expect("clean up test template");
    assert!(failed.is_err(), "first build must report its failure");
    assert!(retry.is_ok(), "retry must start from a clean DB: {retry:?}");
    assert!(rebuilt.load(Ordering::SeqCst), "failed template was reused");
}

#[tokio::test]
async fn template_name_is_published_only_after_successful_build() {
    let template = unique_db_name("proxima_template_visibility");
    let published = template.clone();
    let visible_during_build = Arc::new(AtomicBool::new(true));
    let visible = Arc::clone(&visible_during_build);
    ensure_template(&template, |pool| async move {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)",
        )
        .bind(published)
        .fetch_one(&pool)
        .await?;
        visible.store(exists, Ordering::SeqCst);
        Ok(())
    })
    .await
    .expect("build template");

    let reused = ensure_template(&template, |_| async {
        Err(sqlx::Error::Protocol(
            "completed template was rebuilt".into(),
        ))
    })
    .await;
    drop_db(&template).await.expect("clean up test template");
    assert!(!visible_during_build.load(Ordering::SeqCst));
    assert!(
        reused.is_ok(),
        "completed template must be reused: {reused:?}"
    );
}
