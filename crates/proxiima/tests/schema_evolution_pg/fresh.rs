use super::*;

use proxiima::{Engine, FactWrite};
use proxiima_core::test_fixtures::authenticated_context;
use proxiima_pg_testkit::SplitRoleDb;

#[tokio::test]
async fn fresh_database_migrates_directly_to_v2_before_serving() {
    let db = SplitRoleDb::create("proxiima_evolution_fresh_v2", &[])
        .await
        .expect("PG required");
    let pg = PgMigrationStorage::connect_with_config(
        db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .expect("migration storage");
    let mut registry = SpiiceRegistry::new();
    register_v2(&mut registry).expect("fresh registry");
    let registry = registry.try_freeze().expect("fresh registry freezes");
    let sidecars = sidecars_v2()
        .freeze_against(&registry)
        .expect("fresh sidecars");
    proxiima::run_core_and_spiice_migrations(&pg, AppV2::migrators(), &sidecars)
        .await
        .expect("explicit v2 migration");
    pg.initialize_store_number(0)
        .await
        .expect("store identity initialization");

    let admin = sqlx::PgPool::connect(&db.admin_url())
        .await
        .expect("admin pool");
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT count(*) FROM proxiima_core.memory),
           (SELECT count(*) FROM proxiima_core.goal),
           (SELECT count(*) FROM proxiima_core.blob)",
    )
    .fetch_one(&admin)
    .await
    .expect("empty user graph");
    assert_eq!(counts, (0, 0, 0));
    let completion_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxiima_core.schema_evolution WHERE evolution_id='fact-v1-to-v2'",
    )
    .fetch_one(&admin)
    .await
    .expect("migration completion");
    assert_eq!(completion_count, 1);

    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    sqlx::query("INSERT INTO proxiima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
        .bind(owner.into_inner())
        .execute(&admin)
        .await
        .expect("test owner");
    let pg_storage = super::storage(
        db.runtime_url(),
        db.platform_url(),
        &registry,
        sidecars_v2(),
    )
    .await;
    let engine = Engine::new(registry)
        .try_with_publication_config(PublicationConfig::new(
            PublicationSource::new("urn:proxiima:evolution").expect("publication source"),
        ))
        .expect("publication configuration")
        .with_storage_ports(Arc::new(pg_storage).storage_ports());
    let authz = authenticated_context(AuthzContext::single_owner(&owner, AuthPath::HostBearer));
    let written = engine
        .ingest_fact(
            &authz,
            FactWrite::new(
                owner,
                "fresh-v2-test",
                &FactV2 {
                    task_id: Uuid::now_v7(),
                    ended_at: 14,
                    text: "ordinary target write".into(),
                },
            ),
        )
        .await
        .expect("v2 write");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxiima_core.memory WHERE t=$1")
            .bind(written.memory_id.into_inner())
            .fetch_one(&admin)
            .await
            .expect("written target"),
        1
    );
}
