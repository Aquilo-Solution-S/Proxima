use super::*;

struct AppV2Collision;

#[derive(Debug, PartialEq)]
struct DatabaseSnapshot {
    memories: Vec<(Uuid, String)>,
    sidecars: Vec<(Uuid, Uuid, i64, String)>,
    receipts: Vec<(Uuid, Vec<u8>, Vec<u8>)>,
    ingest_keys: Vec<(Uuid, String, String)>,
    aliases: Vec<(Uuid, Uuid)>,
    v2_rows: i64,
    completions: i64,
    authority_rows: i64,
}

async fn snapshot(admin: &sqlx::PgPool) -> DatabaseSnapshot {
    DatabaseSnapshot {
        memories: sqlx::query_as("SELECT t, schema_id FROM proxiima_core.memory ORDER BY t")
            .fetch_all(admin)
            .await
            .unwrap(),
        sidecars: sqlx::query_as(
            "SELECT t, task_id, ended_at, text FROM evolution_test.fact_v1 ORDER BY t",
        )
        .fetch_all(admin)
        .await
        .unwrap(),
        receipts: sqlx::query_as(
            "SELECT t, envelope, envelope_digest FROM proxiima_core.publication_outbox ORDER BY t",
        )
        .fetch_all(admin)
        .await
        .unwrap(),
        ingest_keys: sqlx::query_as(
            "SELECT t, source_id, ingest_key FROM proxiima_core.ingest_keys ORDER BY t, source_id",
        )
        .fetch_all(admin)
        .await
        .unwrap(),
        aliases: sqlx::query_as(
            "SELECT publication_t, memory_t FROM proxiima_core.schema_evolution_publication ORDER BY publication_t, memory_t",
        )
        .fetch_all(admin)
        .await
        .unwrap(),
        v2_rows: sqlx::query_scalar("SELECT count(*) FROM proxiima_core.memory WHERE schema_id=$1")
            .bind(TARGET_ID)
            .fetch_one(admin)
            .await
            .unwrap(),
        completions: sqlx::query_scalar(
            "SELECT count(*) FROM proxiima_core.schema_evolution WHERE evolution_id=$1",
        )
        .bind("fact-v1-to-v2")
        .fetch_one(admin)
        .await
        .unwrap(),
        authority_rows: sqlx::query_scalar(
            "SELECT count(*) FROM proxiima_core.schema_evolution_authority",
        )
        .fetch_one(admin)
        .await
        .unwrap(),
    }
}

impl SpiiceBundle for AppV2Collision {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register_v2(r)
    }

    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_fact::<FactV2>();
    }

    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, migrator(evolve_collision))]
    }
}

fn evolve_collision(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    for source in input.memories() {
        let Some(old) = source.payload::<FactV1>() else {
            continue;
        };
        let target = plan.fact(
            source,
            FactV2 {
                task_id: Uuid::nil(),
                ended_at: 0,
                text: old.text.clone(),
            },
            MigrationPlacement::NEW,
            MigrationPins::default(),
        )?;
        plan.map_memory(source, target, &[])?;
    }
    Ok(())
}

#[tokio::test]
async fn post_write_receipt_alias_collision_restores_the_v1_database() {
    let seeded = seed_two_v1_facts().await;
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    let before = snapshot(&admin).await;

    let pg = PgMigrationStorage::connect_with_config(
        seeded.db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let mut registry = SpiiceRegistry::new();
    register_v2(&mut registry).unwrap();
    let registry = registry.try_freeze().unwrap();
    let sidecars = sidecars_v2_with(evolve_collision);
    let frozen = sidecars.freeze_against(&registry).unwrap();
    let error = proxiima::run_core_and_spiice_migrations(&pg, AppV2Collision::migrators(), &frozen)
        .await
        .expect_err("the second target's receipt alias must refuse after writes");
    assert!(
        error.to_string().contains("replay") || error.to_string().contains("collision"),
        "{error}"
    );

    let after = snapshot(&admin).await;
    assert_eq!(after.memories, before.memories);
    assert_eq!(after.sidecars, before.sidecars);
    assert_eq!(after.receipts, before.receipts);
    assert_eq!(after.ingest_keys, before.ingest_keys);
    assert_eq!(after.aliases, before.aliases);
    assert_eq!(after.v2_rows, 0);
    assert_eq!(after.completions, 0);
    assert_eq!(after.authority_rows, 0);
}
