use proxiima::spiice::{SpiiceBundle, SpiiceRegistry};
use proxiima::{run_core_and_spiice_migrations, spiice};
use proxiima_storage_pg::{PgMigrationStorage, PgPoolConfig, PgTuning};

pub async fn migrate<A: SpiiceBundle>(
    platform_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let pg = PgMigrationStorage::connect_with_config(
        platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await?;
    let mut registry = SpiiceRegistry::new();
    A::register(&mut registry)?;
    let registry = registry.try_freeze()?;
    let mut sidecars = spiice::PgSidecarRegistry::new();
    spiice::register_core_pg_sidecars(&mut sidecars);
    A::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars.freeze_against(&registry)?;
    run_core_and_spiice_migrations(&pg, A::migrators(), &sidecars).await?;
    pg.clone_pool_for_backend().close().await;
    Ok(())
}

#[allow(dead_code)]
pub async fn migrate_app<A: SpiiceBundle>(platform_url: &str) {
    migrate::<A>(platform_url)
        .await
        .expect("explicit migration");
}
