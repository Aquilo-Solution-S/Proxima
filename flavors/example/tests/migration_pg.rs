//! The example flavor's baseline: the generated trigger text it carries,
//! its owner RLS on a split-role database, and its one tool's owner scope.

use proxima::flavor::{FlavorBundle, FlavorRegistry, PgSidecarRegistry};
use proxima::testkit::{SplitRoleDb, assert_trigger_migrations, scoped_authz};
use proxima::{QueryRequest, company_owner};
use proxima_example::testkit::{build_engine, register_notebook, remember_note};
use proxima_example::{BASELINE_SQL, ExampleFlavor, ExampleStore, FLAVOR_ID, NoteV1};
use uuid::Uuid;

fn frozen_sidecars() -> proxima::flavor::PgSidecarRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    ExampleFlavor::register(&mut registry).expect("the example flavor registers");
    let registry = registry.try_freeze().expect("and freezes");
    let mut sidecars = PgSidecarRegistry::new();
    proxima::flavor::register_core_pg_sidecars(&mut sidecars);
    ExampleFlavor::register_pg_sidecars(&mut sidecars);
    sidecars
        .freeze_against(&registry)
        .expect("the example's PG registrations match its contract")
}

#[test]
fn the_testkit_drift_assertion_holds_for_the_example_flavor() {
    assert_trigger_migrations::<ExampleFlavor>(FLAVOR_ID, &[BASELINE_SQL]);
}

#[test]
fn generated_declaration_triggers_are_the_example_migration_text() {
    let artifacts = frozen_sidecars()
        .declaration_trigger_artifacts(FLAVOR_ID)
        .expect("the example's declaration triggers");
    assert_eq!(artifacts.len(), 2, "one per sidecar");
    for artifact in &artifacts {
        assert!(
            BASELINE_SQL.contains(&artifact.forward),
            "the baseline does not carry the generator's output verbatim:\n{}",
            artifact.forward
        );
    }
}

#[test]
fn generated_presence_triggers_are_the_example_migration_text() {
    let artifacts = frozen_sidecars()
        .presence_trigger_artifacts(FLAVOR_ID)
        .expect("the example's presence triggers");
    assert!(!artifacts.is_empty());
    for artifact in &artifacts {
        assert!(
            BASELINE_SQL.contains(&artifact.forward),
            "the baseline does not carry the generator's output verbatim:\n{}",
            artifact.forward
        );
    }
}

/// The baseline applies on a split-role database, its census passes, and
/// the Engine admits a scoped note into the typed sidecar.
#[tokio::test]
async fn the_baseline_applies_under_split_roles_and_the_census_passes() {
    let db = SplitRoleDb::create("proxima_example_baseline", &[])
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
        "SELECT version FROM public._sqlx_migrations_proxima_example ORDER BY version",
    )
    .fetch_all(&admin)
    .await
    .expect("the example's own ledger");
    assert_eq!(versions, vec![20_261_018_000_011]);
    admin.close().await;
    runtime.close().await;
    built.shutdown().await;
}

/// Owner B's listing does not see owner A's notebook, even on a scope that
/// may read both owners: the tool lists the calling owner only.
#[tokio::test]
async fn the_tool_is_owner_scoped() {
    let db = SplitRoleDb::create("proxima_example_tool", &[])
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
    let both = proxima_core::test_fixtures::owner_scope_for_tests(&[owner_a, owner_b]);
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
