use super::*;

use proxiima::spiice::{EraseMode, SeriesSelection};
use proxiima_core::{Engine, MemoryId};

struct AppV2Retained;

impl SpiiceBundle for AppV2Retained {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register_v2(r)
    }

    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_fact::<FactV2>();
    }

    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, migrator(retain_with_pins))]
    }
}

struct AppV2New;

impl SpiiceBundle for AppV2New {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register_v2(r)
    }

    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_fact::<FactV2>();
    }

    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, migrator(new_with_pins))]
    }
}

fn retain_with_pins(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    for source in input.memories() {
        if let Some(old) = source.payload::<FactV1>() {
            let target = plan.fact(
                source,
                FactV2 {
                    task_id: old.task_id,
                    ended_at: old.ended_at,
                    text: old.text.clone(),
                },
                MigrationPlacement::RETAIN,
                source.pins(),
            )?;
            plan.map_memory(source, target, &[])?;
        }
    }
    Ok(())
}

fn new_with_pins(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    for source in input.memories() {
        if let Some(old) = source.payload::<FactV1>() {
            let target = plan.fact(
                source,
                FactV2 {
                    task_id: old.task_id,
                    ended_at: old.ended_at,
                    text: old.text.clone(),
                },
                MigrationPlacement::NEW,
                source.pins(),
            )?;
            plan.map_memory(source, target, &[])?;
        }
    }
    Ok(())
}

async fn seed_referencing_fact() -> (SeededFacts, Uuid) {
    let seeded = seed_v1_facts(vec![FactV1 {
        task_id: Uuid::now_v7(),
        ended_at: 1,
        text: "erased target".into(),
    }])
    .await;
    let runtime_url = seeded.db.runtime_url().to_owned();
    let platform_url = seeded.db.platform_url().to_owned();
    let mut registry = SpiiceRegistry::new();
    register_v1(&mut registry).unwrap();
    let registry = registry.try_freeze().unwrap();
    let storage = storage(&runtime_url, &platform_url, &registry, sidecars_v1()).await;
    let (engine, system) = Engine::new(registry)
        .try_with_publication_config(PublicationConfig::new(
            PublicationSource::new("urn:proxiima:evolution").unwrap(),
        ))
        .unwrap()
        .with_storage_ports(Arc::new(storage).storage_ports())
        .into_system_authority();
    let authz = authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(seeded.owner, Role::admin())],
        AuthPath::HostBearer,
    ));
    let source = engine
        .ingest_fact(
            &authz,
            FactWrite::new(
                seeded.owner,
                SOURCE_FEED,
                &FactV1 {
                    task_id: Uuid::now_v7(),
                    ended_at: 2,
                    text: "source".into(),
                },
            )
            .refs([MemoryId::new(seeded.ids[0])]),
        )
        .await
        .unwrap()
        .memory_id
        .into_inner();
    let mut erase = engine.system_unit_of_work(&system).unwrap();
    erase
        .erase_own_series(
            SPIICE,
            seeded.owner,
            SeriesSelection::Ids(vec![MemoryId::new(seeded.ids[0])]),
            EraseMode::Erase,
        )
        .await
        .unwrap();
    erase.commit().await.unwrap();
    (seeded, source)
}

async fn run_case(new_identity: bool) {
    let (seeded, source) = seed_referencing_fact().await;
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    let before_outbox: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxiima_core.publication_outbox")
            .fetch_one(&admin)
            .await
            .unwrap();
    let target = seeded.ids[0];
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM proxiima_core.erased_pin_target WHERE t=$1",
        )
        .bind(target)
        .fetch_one(&admin)
        .await
        .unwrap(),
        1
    );

    let platform_url = seeded.db.platform_url().to_owned();
    let pg = PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let mut registry = SpiiceRegistry::new();
    register_v2(&mut registry).unwrap();
    let registry = registry.try_freeze().unwrap();
    let sidecars = if new_identity {
        sidecars_v2_with(new_with_pins)
    } else {
        sidecars_v2_with(retain_with_pins)
    };
    let frozen = sidecars.clone().freeze_against(&registry).unwrap();
    if new_identity {
        proxiima::run_core_and_spiice_migrations(&pg, AppV2New::migrators(), &frozen)
            .await
            .unwrap();
    } else {
        proxiima::run_core_and_spiice_migrations(&pg, AppV2Retained::migrators(), &frozen)
            .await
            .unwrap();
    }

    assert_migrated(&admin, target, source, before_outbox, new_identity).await;

    let active = storage(seeded.db.runtime_url(), &platform_url, &registry, sidecars).await;
    let engine = Engine::new(registry).with_storage_ports(Arc::new(active).storage_ports());
    let authz = authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(seeded.owner, Role::admin())],
        AuthPath::HostBearer,
    ));
    let err = engine
        .ingest_fact(
            &authz,
            FactWrite::new(
                seeded.owner,
                "ordinary-after-erase",
                &FactV2 {
                    task_id: Uuid::now_v7(),
                    ended_at: 3,
                    text: "must refuse".into(),
                },
            )
            .refs([MemoryId::new(target)]),
        )
        .await
        .expect_err("ordinary writes cannot pin an erased target");
    assert!(
        err.to_string().contains("erased")
            || err.to_string().contains("pin")
            || err.to_string().contains("entry not found"),
        "{err}"
    );
}

async fn assert_migrated(
    admin: &sqlx::PgPool,
    target: Uuid,
    source: Uuid,
    before_outbox: i64,
    new_identity: bool,
) -> Uuid {
    let v2_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxiima_core.memory WHERE schema_id=$1")
            .bind(TARGET_ID)
            .fetch_all(admin)
            .await
            .unwrap();
    assert_eq!(v2_ids.len(), 1);
    let migrated = v2_ids[0];
    let source_row: (String, Vec<Uuid>) =
        sqlx::query_as("SELECT schema_id, refs FROM proxiima_core.memory WHERE t=$1")
            .bind(migrated)
            .fetch_one(admin)
            .await
            .unwrap();
    assert_eq!(source_row.0, TARGET_ID);
    assert_eq!(source_row.1, vec![target]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM evolution_test.fact_v2 WHERE t=$1")
            .bind(migrated)
            .fetch_one(admin)
            .await
            .unwrap(),
        1
    );
    if new_identity {
        assert_ne!(migrated, source);
    } else {
        assert_eq!(migrated, source);
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM proxiima_core.erased_pin_target WHERE t=$1"
        )
        .bind(target)
        .fetch_one(admin)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM proxiima_core.erased_pin_target WHERE t=$1"
        )
        .bind(source)
        .fetch_one(admin)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM proxiima_core.publication_outbox")
            .fetch_one(admin)
            .await
            .unwrap(),
        before_outbox
    );
    migrated
}

#[tokio::test]
async fn retained_migration_preserves_erased_pin_and_witness() {
    run_case(false).await;
}

#[tokio::test]
async fn new_migration_preserves_erased_pin_and_witness() {
    run_case(true).await;
}
