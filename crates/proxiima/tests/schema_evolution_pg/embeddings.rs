use super::*;

use proxiima::spiice::SpiiceBundle;
use proxiima_core::payload::SearchProjectionColumnKind;
use proxiima_core::spiice::{BandComparability, ProjectionSpec, RankSource};
use proxiima_core::spiice::{EmbedUnit, EmbeddingRecipe, SLOT_DEFAULT};
use proxiima_core::spiice::{LanguagePolicy, SubstringArm, WEIGHT_UNIFORM, WeightedField};
use sqlx::migrate::{Migration, MigrationType, Migrator};
use std::borrow::Cow;

const EMBEDDED_PROJECTION: ProjectionSpec = ProjectionSpec {
    table: "evolution_test.projection",
    index: "evolution_test_projection_owner_tsv_gin",
    overfetch_k: 1000,
    band_comparability: BandComparability::CoreBands,
    rank_source: RankSource::Projection,
};
const EMBEDDED_ANCHOR: i64 = ANCHOR + 1;

static V2_EMBEDDED: SchemaContract = SchemaContract {
    id: SchemaRef::new(SPIICE, "fact", 2),
    kind: PayloadKind::Fact,
    sidecar_table: Some(TABLE_V2),
    search: SearchProjectionDecl::Projected {
        fields: &[WeightedField {
            column: "text",
            kind: SearchProjectionColumnKind::Text,
            weight: WEIGHT_UNIFORM,
        }],
        tag_column: None,
        language: LanguagePolicy::Pinned("simple"),
        bands: &[
            proxiima_core::spiice0::BAND_EXACT,
            proxiima_core::spiice0::BAND_RESCUE,
            proxiima_core::spiice0::BAND_SUBSTRING,
        ],
        substring: SubstringArm::MemoryFirstNestedLoop,
    },
    embedding: EmbeddingRecipe::Units(&[EmbedUnit::stored("text", SLOT_DEFAULT)]),
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[FACT_V2_SURFACE],
    natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
};

static EVOLUTION_EMBEDDED: SchemaEvolution = SchemaEvolution::new(
    "fact-v1-to-v2-embedded",
    &[V1],
    &[V2_EMBEDDED],
    EMBEDDED_ANCHOR,
    &[],
    "change fact representation with embeddings",
    "redeploy the v2 consumer",
);
static CONTRACT_V2_EMBEDDED: SpiiceContract = SpiiceContract {
    spiice_id: SPIICE,
    ordinal: 42,
    schemas: &[V2_EMBEDDED],
    evolutions: &[EVOLUTION_EMBEDDED],
    retired_facts: &[],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::Table(EMBEDDED_PROJECTION),
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

struct AppV2Embedded;

impl SpiiceBundle for AppV2Embedded {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        r.try_add_spiice(SpiiceDescriptor {
            spiice_id: SPIICE.into(),
            display_name: SPIICE.into(),
            package_version: "0".into(),
            author: None,
            provenance: SpiiceProvenance::Builtin,
        })?;
        r.try_add_fact_schema::<FactV2>()?;
        r.try_add_contract(&CONTRACT_V2_EMBEDDED)
    }

    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_fact::<FactV2>();
    }

    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, embedded_migrator())]
    }
}

fn embedded_migrator() -> Migrator {
    let mut migrations = migrator(embedded_transform);
    let projection = proxiima::spiice::projection_artifacts(&CONTRACT_V2_EMBEDDED)
        .unwrap()
        .unwrap();
    migrations.migrations.to_mut().push(Migration::new(
        EMBEDDED_ANCHOR,
        Cow::Borrowed("evolution test projection"),
        MigrationType::Simple,
        // SQL-POLICY: generated — the projection declaration owns all DDL.
        sqlx::AssertSqlSafe(projection.render()).into_sql_str(),
        false,
    ));
    migrations
}

fn embedded_transform(
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

fn embedded_sidecars() -> PgSidecarRegistry {
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<FactV2>();
    let mut schemas = PgMigrationSchemas::new();
    schemas.add_fact::<FactV1>();
    schemas.add_fact::<FactV2>();
    sidecars.add_evolution(PgSchemaEvolution::new(
        &EVOLUTION_EMBEDDED,
        schemas,
        embedded_transform,
    ));
    sidecars
}

async fn seed_derived_rows(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    memory_id: Uuid,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxiima_core.embeddings
         (entity_id, model_id, dim, embedding_version, vec, owner_id)
         VALUES ($1, 'test-embed', 1024, 1, ARRAY_FILL(0.0::real, ARRAY[1024])::vector, $2)",
    )
    .bind(memory_id)
    .bind(owner.into_inner())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.embedding_heads
         (entity_id, model_id, dim, embedding_version, owner_id)
         VALUES ($1, 'test-embed', 1024, 1, $2)",
    )
    .bind(memory_id)
    .bind(owner.into_inner())
    .execute(pool)
    .await?;
    let claim = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxiima_core.embedding_jobs
         (entity_id, model_id, dim, owner_id, status, claimed_at, claim_token)
         VALUES ($1, 'test-embed', 1024, $2, $3::proxiima_core.embedding_job_status,
                 CASE WHEN $3 = 'processing' THEN now() END, CASE WHEN $3 = 'processing' THEN $4 END)",
    )
    .bind(memory_id)
    .bind(owner.into_inner())
    .bind(status)
    .bind(claim)
    .execute(pool)
    .await?;
    Ok(())
}

async fn migrate_embedded(seeded: &SeededFacts) -> Result<(), Box<dyn std::error::Error>> {
    let pg = PgMigrationStorage::connect_with_config(
        seeded.db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await?;
    let mut registry = SpiiceRegistry::new();
    AppV2Embedded::register(&mut registry)?;
    let registry = registry.try_freeze()?;
    let sidecars = embedded_sidecars().freeze_against(&registry)?;
    proxiima::run_core_and_spiice_migrations(&pg, AppV2Embedded::migrators(), &sidecars).await?;
    Ok(())
}

#[tokio::test]
async fn embedded_migration_invalidates_sources_and_requeues_each_space_once() {
    let seeded = seed_v1_facts(vec![FactV1 {
        task_id: Uuid::now_v7(),
        ended_at: 11,
        text: "embedding source".into(),
    }])
    .await;
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    seed_derived_rows(&admin, seeded.owner, seeded.ids[0], "processing")
        .await
        .unwrap();
    migrate_embedded(&seeded).await.unwrap();
    let target: Uuid = sqlx::query_scalar("SELECT t FROM proxiima_core.memory WHERE schema_id=$1")
        .bind(TARGET_ID)
        .fetch_one(&admin)
        .await
        .unwrap();
    let source_counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT count(*) FROM proxiima_core.embeddings WHERE entity_id=$1),
           (SELECT count(*) FROM proxiima_core.embedding_heads WHERE entity_id=$1),
           (SELECT count(*) FROM proxiima_core.embedding_jobs WHERE entity_id=$1)",
    )
    .bind(seeded.ids[0])
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(source_counts, (0, 0, 1));
    let target_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxiima_core.embedding_jobs WHERE entity_id=$1 AND model_id='test-embed'",
    )
    .bind(target)
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(target_jobs, 1);
    let target_projection: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM evolution_test.projection WHERE memory_id=$1 AND schema_id=$2",
    )
    .bind(target)
    .bind(TARGET_ID)
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(target_projection, 1, "live migrated target is projected");
    let status: String = sqlx::query_scalar(
        "SELECT status::text FROM proxiima_core.embedding_jobs
         WHERE entity_id=$1 AND model_id='test-embed'",
    )
    .bind(target)
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(status, "pending");
}

#[tokio::test]
async fn forgotten_embedded_target_is_not_requeued() {
    let seeded = seed_v1_facts(vec![FactV1 {
        task_id: Uuid::now_v7(),
        ended_at: 12,
        text: "forgotten embedding".into(),
    }])
    .await;
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    sqlx::query("UPDATE proxiima_core.memory SET forgotten_at=now() WHERE t=$1")
        .bind(seeded.ids[0])
        .execute(&admin)
        .await
        .unwrap();
    seed_derived_rows(&admin, seeded.owner, seeded.ids[0], "pending")
        .await
        .unwrap();
    migrate_embedded(&seeded).await.unwrap();
    let target: Uuid = sqlx::query_scalar("SELECT t FROM proxiima_core.memory WHERE schema_id=$1")
        .bind(TARGET_ID)
        .fetch_one(&admin)
        .await
        .unwrap();
    let target_forgotten: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxiima_core.memory WHERE t=$1 AND forgotten_at IS NOT NULL",
    )
    .bind(target)
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(target_forgotten, 1);
    let target_projection: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM evolution_test.projection WHERE memory_id=$1 AND schema_id=$2",
    )
    .bind(target)
    .bind(TARGET_ID)
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(
        target_projection, 0,
        "forgotten migrated target has no projection"
    );
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM proxiima_core.embedding_jobs")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(jobs, 0);
}

#[tokio::test]
async fn never_target_is_not_requeued_after_source_invalidation() {
    let seeded = seed_v1_facts(vec![FactV1 {
        task_id: Uuid::now_v7(),
        ended_at: 13,
        text: "never embedding".into(),
    }])
    .await;
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    seed_derived_rows(&admin, seeded.owner, seeded.ids[0], "processing")
        .await
        .unwrap();
    let pg = PgMigrationStorage::connect_with_config(
        seeded.db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let mut registry = SpiiceRegistry::new();
    AppV2::register(&mut registry).unwrap();
    let registry = registry.try_freeze().unwrap();
    let sidecars = sidecars_v2().freeze_against(&registry).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppV2::migrators(), &sidecars)
        .await
        .unwrap();
    let counts: (i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT count(*) FROM proxiima_core.embeddings WHERE entity_id=$1),
           (SELECT count(*) FROM proxiima_core.embedding_heads WHERE entity_id=$1),
           (SELECT count(*) FROM proxiima_core.embedding_jobs WHERE entity_id=$1)",
    )
    .bind(seeded.ids[0])
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(counts, (0, 0, 0));
}
