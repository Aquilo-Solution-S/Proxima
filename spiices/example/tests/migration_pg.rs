//! The example spiice's migrations: generated trigger text, owner RLS on a
//! split-role database, and its one tool's owner scope.

use proxiima::spiice::{PgSidecarRegistry, SpiiceBundle, SpiiceRegistry};
use proxiima::testkit::{SplitRoleDb, assert_trigger_migrations, scoped_authz};
use proxiima::{QueryRequest, company_owner};
use proxiima_example::testkit::{build_engine, register_notebook, remember_note};
use proxiima_example::{
    BASELINE_SQL, ExampleSpiice, ExampleStore, NoteV1, SPIICE_ID, STAMP_FOLLOWS_SQL,
};
use uuid::Uuid;

fn frozen_sidecars() -> proxiima::spiice::PgSidecarRegistryFrozen {
    let mut registry = SpiiceRegistry::new();
    ExampleSpiice::register(&mut registry).expect("the example spiice registers");
    let registry = registry.try_freeze().expect("and freezes");
    let mut sidecars = PgSidecarRegistry::new();
    proxiima::spiice::register_core_pg_sidecars(&mut sidecars);
    ExampleSpiice::register_pg_sidecars(&mut sidecars);
    sidecars
        .freeze_against(&registry)
        .expect("the example's PG registrations match its contract")
}

#[test]
fn the_testkit_drift_assertion_holds_for_the_example_spiice() {
    assert_trigger_migrations::<ExampleSpiice>(SPIICE_ID, &[BASELINE_SQL, STAMP_FOLLOWS_SQL]);
}

#[test]
fn generated_declaration_triggers_are_the_example_migration_text() {
    let artifacts = frozen_sidecars()
        .declaration_trigger_artifacts(SPIICE_ID)
        .expect("the example's declaration triggers");
    assert_eq!(artifacts.len(), 3, "one per sidecar");
    for artifact in &artifacts {
        assert!(
            BASELINE_SQL.contains(&artifact.forward)
                || STAMP_FOLLOWS_SQL.contains(&artifact.forward),
            "no example migration carries the generator's output verbatim:\n{}",
            artifact.forward
        );
    }
}

#[test]
fn generated_presence_triggers_are_the_example_migration_text() {
    let artifacts = frozen_sidecars()
        .presence_trigger_artifacts(SPIICE_ID)
        .expect("the example's presence triggers");
    assert!(!artifacts.is_empty());
    for artifact in &artifacts {
        assert!(
            BASELINE_SQL.contains(&artifact.forward)
                || STAMP_FOLLOWS_SQL.contains(&artifact.forward),
            "no example migration carries the generator's output verbatim:\n{}",
            artifact.forward
        );
    }
}

/// The example migrator applies on a split-role database, its census passes,
/// and the Engine admits a scoped note into the typed sidecar.
#[tokio::test]
async fn the_baseline_applies_under_split_roles_and_the_census_passes() {
    let db = SplitRoleDb::create("proxiima_example_baseline", &[])
        .await
        .expect("PG required");
    let owner = company_owner(Uuid::now_v7());
    let built = build_engine(db.runtime_url(), db.platform_url(), owner)
        .await
        .expect("the example baseline boots under the runtime RLS guard");
    let authz = scoped_authz(owner);
    let runtime = sqlx::PgPool::connect(db.runtime_url()).await.unwrap();
    let notebook_id = register_notebook(
        &runtime,
        authz.owner_scope().expect("owner scope"),
        owner,
        "inbox",
    )
    .await
    .expect("the owner registers a notebook");
    let engine = built.host().engine();
    let memory_id = remember_note(
        engine,
        &authz,
        owner,
        &NoteV1 {
            notebook_id,
            key: "first".to_owned(),
            body: "a note under owner RLS".to_owned(),
            written_at: time::OffsetDateTime::now_utc(),
        },
    )
    .await
    .expect("a note in a registered notebook is admitted");
    let response = engine
        .query(&authz, &QueryRequest::readable())
        .await
        .expect("owner query");
    assert!(
        response
            .memories
            .iter()
            .any(|memory| memory.id == memory_id)
    );

    let admin = sqlx::PgPool::connect(&db.admin_url()).await.unwrap();
    let versions: Vec<i64> = sqlx::query_scalar(
        "SELECT version FROM public._sqlx_migrations_proxiima_example ORDER BY version",
    )
    .fetch_all(&admin)
    .await
    .expect("the example's own ledger");
    assert_eq!(
        versions,
        proxiima_example::migrator()
            .iter()
            .map(|migration| migration.version)
            .collect::<Vec<_>>()
    );
    admin.close().await;
    runtime.close().await;
    built.shutdown().await;
}

/// Owner B's listing does not see owner A's notebook, even on a scope that
/// may read both owners: the tool lists the calling owner only.
#[tokio::test]
async fn the_tool_is_owner_scoped() {
    let db = SplitRoleDb::create("proxiima_example_tool", &[])
        .await
        .expect("PG required");
    let owner_a = company_owner(Uuid::now_v7());
    let owner_b = company_owner(Uuid::now_v7());
    let built = build_engine(db.runtime_url(), db.platform_url(), owner_a)
        .await
        .expect("boot");
    let runtime = sqlx::PgPool::connect(db.runtime_url()).await.unwrap();
    let authz_a = scoped_authz(owner_a);
    let scope_a = authz_a.owner_scope().expect("scope a");
    let notebook_a = register_notebook(&runtime, scope_a, owner_a, "a's")
        .await
        .expect("a registers");
    let both = proxiima::testkit::owner_scope_for_tests(&[owner_a, owner_b]);
    let store = ExampleStore::from_host_pool(runtime.clone());
    assert_eq!(
        store
            .list_notebooks(&both, owner_a.into_inner())
            .await
            .expect("a lists"),
        vec![(notebook_a, "a's".to_owned())],
        "the shared scope reads owner A's notebook as owner A"
    );
    assert_eq!(
        store
            .list_notebooks(&both, owner_b.into_inner())
            .await
            .expect("b lists"),
        Vec::new(),
        "owner B must not see owner A's notebook"
    );
    runtime.close().await;
    built.shutdown().await;
}
