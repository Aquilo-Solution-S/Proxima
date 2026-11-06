use super::*;

#[tokio::test]
async fn chain_cleanup_leaves_standard_rls_and_reruns_after_old_tables_are_gone() {
    let seeded = seed_v1_facts(vec![FactV1 {
        task_id: Uuid::now_v7(),
        ended_at: 12,
        text: "cleanup-chain".into(),
    }])
    .await;
    let platform_url = seeded.db.platform_url().to_owned();
    let pg = PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let mut registry = SpiiceRegistry::new();
    register_v3(&mut registry).unwrap();
    let registry = registry.try_freeze().unwrap();
    let sidecars = sidecars_v3();
    let frozen = sidecars.freeze_against(&registry).unwrap();

    proxiima::run_core_and_spiice_migrations(&pg, AppV3::migrators(), &frozen)
        .await
        .unwrap();
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    let old_tables: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT to_regclass('evolution_test.fact_v1')::text, to_regclass('evolution_test.fact_v2')::text",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(old_tables, (None, None));
    let (v3_rls, v3_policy): (bool, i64) = sqlx::query_as(
        "SELECT c.relrowsecurity, (SELECT count(*) FROM pg_policies WHERE schemaname='evolution_test' AND tablename='fact_v3') FROM pg_class c WHERE c.oid='evolution_test.fact_v3'::regclass",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert!(v3_rls);
    assert!(v3_policy > 0);

    proxiima::run_core_and_spiice_migrations(&pg, AppV3::migrators(), &frozen)
        .await
        .unwrap();
    let completed: Vec<String> = sqlx::query_scalar(
        "SELECT evolution_id FROM proxiima_core.schema_evolution ORDER BY evolution_id",
    )
    .fetch_all(&admin)
    .await
    .unwrap();
    assert_eq!(completed, vec!["fact-v1-to-v2", "fact-v2-to-v3"]);
}
