use super::*;

pub(super) async fn assert_split_capture_erasure(seeded: &SeededFacts, targets: &[Uuid]) {
    let mut registry = SpiiceRegistry::new();
    register_v2(&mut registry).unwrap();
    let registry = registry.try_freeze().unwrap();
    let active = storage(
        seeded.db.runtime_url(),
        seeded.db.platform_url(),
        &registry,
        sidecars_v2_with(evolve_split),
    )
    .await;
    let (engine, authority) = proxiima_core::Engine::new(registry)
        .with_storage_ports(Arc::new(active).storage_ports())
        .into_system_authority();
    let mut erase = engine.system_unit_of_work(&authority).unwrap();
    erase
        .erase_own_series(
            SPIICE,
            seeded.owner,
            proxiima::spiice::SeriesSelection::Ids(vec![proxiima_core::MemoryId::new(targets[0])]),
            proxiima::spiice::EraseMode::Erase,
        )
        .await
        .unwrap();
    erase.commit().await.unwrap();
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    let remaining: Vec<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxiima_core.memory WHERE schema_id=$1 ORDER BY t")
            .bind(TARGET_ID)
            .fetch_all(&admin)
            .await
            .unwrap();
    assert_eq!(remaining, [targets[1]], "the other split series survives");
    let captures: (i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM proxiima_core.publication_outbox),
                (SELECT count(*) FROM proxiima_core.publication_origin),
                (SELECT count(*) FROM proxiima_core.schema_evolution_publication)",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(
        captures,
        (0, 0, 0),
        "the shared capture contains the erased source bytes"
    );
    admin.close().await;
}
