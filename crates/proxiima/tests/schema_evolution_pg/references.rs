//! Cross-owner reference remapping through two executable schema steps.

use super::*;

fn evolve_new(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    for source in input.memories() {
        let old = source
            .payload::<FactV1>()
            .ok_or_else(|| proxiima_core::StorageError::ConstraintViolation("FactV1".into()))?;
        let target = plan.fact(
            source,
            FactV2 {
                task_id: old.task_id,
                ended_at: old.ended_at,
                text: old.text.clone(),
            },
            proxiima::spiice::MigrationPlacement::NEW,
            source.pins(),
        )?;
        plan.map_memory(source, target, &[])?;
    }
    Ok(())
}

fn evolve_reference(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    for source in input.memories() {
        let old = source
            .payload::<FactV2>()
            .ok_or_else(|| proxiima_core::StorageError::ConstraintViolation("FactV2".into()))?;
        let mut pins = source.pins();
        if old.text == "owner B" {
            assert!(
                input.mapped_targets(old.task_id).is_none(),
                "an unrelated foreign mapping stays private"
            );
            assert!(
                input
                    .mapped_targets(pins.references[0].into_inner())
                    .is_some(),
                "the existing reference exposes its replacement mapping"
            );
        }
        pins.references = pins
            .references
            .iter()
            .map(|id| {
                input
                    .mapped_one(id.into_inner())
                    .map(proxiima_core::MemoryId::new)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let target = plan.fact(
            source,
            FactV3 {
                task_id: old.task_id,
                ended_at: old.ended_at,
                text: old.text.clone(),
            },
            proxiima::spiice::MigrationPlacement::RETAIN,
            pins,
        )?;
        plan.map_memory(source, target, &[])?;
    }
    Ok(())
}

fn sidecars_v3_with_reference() -> PgSidecarRegistry {
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    AppV3::register_pg_sidecars(&mut sidecars);
    let mut first = PgMigrationSchemas::new();
    first.add_fact::<FactV1>();
    first.add_fact::<FactV2>();
    sidecars.add_evolution(PgSchemaEvolution::new(&EVOLUTION, first, evolve_new));
    let mut second = PgMigrationSchemas::new();
    second.add_fact::<FactV2>();
    second.add_fact::<FactV3>();
    sidecars.add_evolution(PgSchemaEvolution::new(
        &EVOLUTION_V2_TO_V3,
        second,
        evolve_reference,
    ));
    sidecars
}

#[tokio::test]
async fn dependent_owner_rewrites_cross_owner_reference_from_prior_mapping() {
    let seeded = seed_v1_facts(vec![
        FactV1 {
            task_id: Uuid::now_v7(),
            ended_at: 1,
            text: "owner A".into(),
        },
        FactV1 {
            task_id: Uuid::now_v7(),
            ended_at: 2,
            text: "unrelated owner A record".into(),
        },
    ])
    .await;
    let db = &seeded.db;
    let owner_a = seeded.owner;
    let source_a = proxiima_core::MemoryId::new(seeded.ids[0]);
    let owner_b = OwnerId::new(Uuid::now_v7());
    let admin = sqlx::PgPool::connect(&db.admin_url()).await.unwrap();
    sqlx::query("INSERT INTO proxiima_core.owners (owner_id,kind) VALUES ($1,'personal')")
        .bind(owner_b.into_inner())
        .execute(&admin)
        .await
        .unwrap();
    let source_b = Uuid::now_v7();
    let handle_b = Uuid::now_v7();
    let task_b = seeded.ids[1];
    let mut admin_tx = admin.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.proxiima_scope','platform',true)")
        .execute(&mut *admin_tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO proxiima_core.memory_head(handle,kind,schema_id,owner_id,t) VALUES($1,'fact',$2,$3,$4)")
        .bind(handle_b).bind("evolution-test/fact-v1").bind(owner_b.into_inner()).bind(source_b)
        .execute(&mut *admin_tx).await.unwrap();
    sqlx::query("INSERT INTO proxiima_core.memory(handle,t,kind,owner_id,schema_id,refs,sidecar_tables,occurred_at,\"by\") VALUES($1,$2,'fact',$3,$4,ARRAY[$5]::uuid[],ARRAY[$6]::text[],now(),$3)")
        .bind(handle_b).bind(source_b).bind(owner_b.into_inner()).bind("evolution-test/fact-v1").bind(source_a.into_inner()).bind(TABLE_V1)
        .execute(&mut *admin_tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO evolution_test.fact_v1(t,task_id,ended_at,text) VALUES($1,$2,2,'owner B')",
    )
    .bind(source_b)
    .bind(task_b)
    .execute(&mut *admin_tx)
    .await
    .unwrap();
    admin_tx.commit().await.unwrap();

    let mut r3 = SpiiceRegistry::new();
    register_v3(&mut r3).unwrap();
    let r3 = r3.try_freeze().unwrap();
    let s3 = sidecars_v3_with_reference();
    let f3 = s3.freeze_against(&r3).unwrap();
    let pg = PgMigrationStorage::connect_with_config(
        db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    proxiima::run_core_and_spiice_migrations(
        &pg,
        vec![NamedMigrator::new(SPIICE, chain_migrator())],
        &f3,
    )
    .await
    .unwrap();
    let map_a: Uuid = sqlx::query_scalar("SELECT m.t FROM proxiima_core.memory m JOIN evolution_test.fact_v3 payload ON payload.t=m.t WHERE m.owner_id=$1 AND payload.text='owner A'")
        .bind(owner_a.into_inner()).fetch_one(&admin).await.unwrap();
    let final_b: Uuid = sqlx::query_scalar("SELECT t FROM proxiima_core.memory WHERE owner_id=$1 AND schema_id='evolution-test/fact-v3'")
        .bind(owner_b.into_inner()).fetch_one(&admin).await.unwrap();
    let refs: Vec<Uuid> = sqlx::query_scalar("SELECT refs FROM proxiima_core.memory WHERE t=$1")
        .bind(final_b)
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(
        refs,
        vec![map_a],
        "dependent owner follows the referenced owner's replacement"
    );
    assert_ne!(map_a, source_a.into_inner());
    assert_ne!(final_b, source_b);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM proxiima_core.memory WHERE t=ANY($1::uuid[])"
        )
        .bind(vec![source_a.into_inner(), source_b])
        .fetch_one(&admin)
        .await
        .unwrap(),
        0
    );
}
