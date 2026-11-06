//! One end-to-end receipt for a typed Fact representation migration.

#[path = "schema_evolution_pg/cleanup.rs"]
mod cleanup;
#[path = "schema_evolution_pg/erasure.rs"]
mod erasure;
#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

use proxiima::FactWrite;
use proxiima::spiice::{
    FactPayload, GoalAuthorship, GoalCreateRequest, GoalTopologyWrite, IdempotencyKey,
    MigrationPins, MigrationPlacement, NamedMigrator, PayloadKeyBuilder, PgMigrationSchemas,
    PgSchemaEvolution, SpiiceBundle, SpiiceDescriptor, SpiiceProvenance, SpiiceRegistry,
    SpiiceRegistryError,
};
use proxiima_core::publication::{PublicationConfig, PublicationSource};
use proxiima_core::spiice::{
    CounterRule, DbConstraint, EmbeddingRecipe, EraseRule, ExportRule, ForgetRule, KeyShape,
    ProjectionDecl, Provenance, SchemaContract, SchemaEvolution, SchemaRef, SearchProjectionDecl,
    SpiiceContract, Surface, TransferRule,
};
use proxiima_core::test_fixtures::authenticated_context;
use proxiima_core::verbs::schema::PayloadKind;
use proxiima_core::{AuthPath, AuthzContext, GoalHandle, OwnerId, Role, SimpleTextGoalV1, UserId};
use proxiima_storage_pg::{
    PgMigrationStorage, PgPlatformScope, PgPoolConfig, PgSidecarRegistry, PgStorage, PgTuning,
    register_core_pg_sidecars,
};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use std::borrow::Cow;
use std::sync::Arc;
use uuid::Uuid;

#[path = "schema_evolution_pg/embeddings.rs"]
mod embeddings;
#[path = "schema_evolution_pg/fresh.rs"]
mod fresh;
#[path = "schema_evolution_pg/references.rs"]
mod references;
#[path = "schema_evolution_pg/retained.rs"]
mod retained;
#[path = "schema_evolution_pg/rollback.rs"]
mod rollback;

const SPIICE: &str = "evolution-test";
const SOURCE_FEED: &str = "evolution-test/task-completions";
const TARGET_ID: &str = "evolution-test/fact-v2";
const TABLE_V1: &str = "evolution_test.fact_v1";
const TABLE_V2: &str = "evolution_test.fact_v2";
const TABLE_V3: &str = "evolution_test.fact_v3";
const ANCHOR: i64 = 2_026_110_600_001;
const ANCHOR_V3: i64 = 2_026_110_600_002;
const CLEANUP_ANCHOR: i64 = 2_026_110_600_003;

static FACT_V1_SURFACE: Surface = Surface {
    table: TABLE_V1,
    key: KeyShape::MemoryT { column: "t" },
    owner_column: None,
    transfer: TransferRule::StaysOnKey,
    erase: EraseRule::ByKey,
    export: ExportRule::Rows,
    forget: ForgetRule::DumpThenDelete,
    lexical_language_column: None,
    counter: CounterRule::Counted("sidecar_rows"),
    completeness: Some(DbConstraint {
        relation: TABLE_V1,
        name: "fact_v1_t_fkey",
    }),
};
static FACT_V2_SURFACE: Surface = Surface {
    table: TABLE_V2,
    key: KeyShape::MemoryT { column: "t" },
    owner_column: None,
    transfer: TransferRule::StaysOnKey,
    erase: EraseRule::ByKey,
    export: ExportRule::Rows,
    forget: ForgetRule::DumpThenDelete,
    lexical_language_column: None,
    counter: CounterRule::Counted("sidecar_rows"),
    completeness: Some(DbConstraint {
        relation: TABLE_V2,
        name: "fact_v2_t_fkey",
    }),
};
static FACT_V3_SURFACE: Surface = Surface {
    table: TABLE_V3,
    key: KeyShape::MemoryT { column: "t" },
    owner_column: None,
    transfer: TransferRule::StaysOnKey,
    erase: EraseRule::ByKey,
    export: ExportRule::Rows,
    forget: ForgetRule::DumpThenDelete,
    lexical_language_column: None,
    counter: CounterRule::Counted("sidecar_rows"),
    completeness: Some(DbConstraint {
        relation: TABLE_V3,
        name: "fact_v3_t_fkey",
    }),
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FactV1 {
    task_id: Uuid,
    ended_at: i64,
    text: String,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FactV2 {
    task_id: Uuid,
    ended_at: i64,
    text: String,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FactV3 {
    task_id: Uuid,
    ended_at: i64,
    text: String,
}

macro_rules! fact_impl {
    ($ty:ty, $id:literal, $version:literal, $table:literal) => {
        impl FactPayload for $ty {
            const SCHEMA_ID: &'static str = $id; const SCHEMA_VERSION: u32 = $version; const LISTENABLE: bool = true;
            fn receipt_key(&self) -> Vec<u8> { let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION); key.field_uuid("task_id", self.task_id); key.field_i64("ended_at", self.ended_at); key.finish() }
            fn render(&self) -> String { self.text.clone() }
            fn sidecar_table() -> Option<&'static str> { Some($table) }
            fn json_schema() -> Option<serde_json::Value> { Some(serde_json::json!({"type":"object","required":["task_id","ended_at","text"],"properties":{"task_id":{"type":"string"},"ended_at":{"type":"integer"},"text":{"type":"string"}}})) }
        }
        impl proxiima::spiice::PayloadProjection for $ty {
            const KEY_FIELDS: &'static [proxiima::spiice::KeyField<Self>] = &[];
            fn display(&self) -> proxiima::spiice::PayloadDisplay { proxiima::spiice::PayloadDisplay { body: Some(self.text.clone()), ..Default::default() } }
            fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> { let mut b = proxiima::spiice::DurablePayloadBuilder::new(); b.field("task_id", &self.task_id)?.field("ended_at", &self.ended_at)?.field("text", &self.text)?; Ok(b.finish()) }
        }
    }
}
fact_impl!(
    FactV1,
    "evolution-test/fact-v1",
    1,
    "evolution_test.fact_v1"
);
fact_impl!(
    FactV3,
    "evolution-test/fact-v3",
    3,
    "evolution_test.fact_v3"
);
fact_impl!(
    FactV2,
    "evolution-test/fact-v2",
    2,
    "evolution_test.fact_v2"
);
proxiima::spiice::pg_sidecar! { payload: FactV1, row: FactV1Row, kinds: [Fact], table: "evolution_test.fact_v1", key: t, fields: { task_id => task_id: (uuid), ended_at => ended_at: (i64), text => text: (text), } }
proxiima::spiice::pg_sidecar! { payload: FactV2, row: FactV2Row, kinds: [Fact], table: "evolution_test.fact_v2", key: t, fields: { task_id => task_id: (uuid), ended_at => ended_at: (i64), text => text: (text), } }
proxiima::spiice::pg_sidecar! { payload: FactV3, row: FactV3Row, kinds: [Fact], table: "evolution_test.fact_v3", key: t, fields: { task_id => task_id: (uuid), ended_at => ended_at: (i64), text => text: (text), } }

static V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(SPIICE, "fact", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some(TABLE_V1),
    search: SearchProjectionDecl::None {
        why: "evolution receipt",
        tag_column: None,
    },
    embedding: EmbeddingRecipe::Never {
        why: "evolution receipt",
    },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[FACT_V1_SURFACE],
    natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
};
static V2: SchemaContract = SchemaContract {
    id: SchemaRef::new(SPIICE, "fact", 2),
    kind: PayloadKind::Fact,
    sidecar_table: Some(TABLE_V2),
    search: SearchProjectionDecl::None {
        why: "evolution receipt",
        tag_column: None,
    },
    embedding: EmbeddingRecipe::Never {
        why: "evolution receipt",
    },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[FACT_V2_SURFACE],
    natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
};
static V3: SchemaContract = SchemaContract {
    id: SchemaRef::new(SPIICE, "fact", 3),
    kind: PayloadKind::Fact,
    sidecar_table: Some(TABLE_V3),
    search: SearchProjectionDecl::None {
        why: "evolution receipt",
        tag_column: None,
    },
    embedding: EmbeddingRecipe::Never {
        why: "evolution receipt",
    },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[FACT_V3_SURFACE],
    natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
};
static EVOLUTION: SchemaEvolution = SchemaEvolution::new(
    "fact-v1-to-v2",
    &[V1],
    &[V2],
    ANCHOR,
    &[],
    "change fact representation",
    "redeploy the v2 consumer",
);
static EVOLUTION_V2_TO_V3: SchemaEvolution = SchemaEvolution::new(
    "fact-v2-to-v3",
    &[V2],
    &[V3],
    ANCHOR_V3,
    &[],
    "change fact representation again",
    "redeploy the v3 consumer",
);
static CONTRACT_V1: SpiiceContract = SpiiceContract {
    spiice_id: SPIICE,
    ordinal: 42,
    schemas: &[V1],
    evolutions: &[],
    retired_facts: &[],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None {
        why: "evolution receipt",
    },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};
static CONTRACT_V2: SpiiceContract = SpiiceContract {
    spiice_id: SPIICE,
    ordinal: 42,
    schemas: &[V2],
    evolutions: &[EVOLUTION],
    retired_facts: &[],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None {
        why: "evolution receipt",
    },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};
static CONTRACT_V3: SpiiceContract = SpiiceContract {
    spiice_id: SPIICE,
    ordinal: 42,
    schemas: &[V3],
    evolutions: &[EVOLUTION, EVOLUTION_V2_TO_V3],
    retired_facts: &[],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None {
        why: "evolution receipt",
    },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};
static CONTRACT_MIGRATOR: SpiiceContract = SpiiceContract {
    spiice_id: SPIICE,
    ordinal: 42,
    schemas: &[V1, V2],
    evolutions: &[EVOLUTION],
    retired_facts: &[],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None {
        why: "evolution receipt",
    },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

fn register_v1(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
    r.try_add_spiice(SpiiceDescriptor {
        spiice_id: SPIICE.into(),
        display_name: SPIICE.into(),
        package_version: "0".into(),
        author: None,
        provenance: SpiiceProvenance::Builtin,
    })?;
    r.try_add_fact_schema::<FactV1>()?;
    r.try_add_contract(&CONTRACT_V1)
}
fn register_v2(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
    r.try_add_spiice(SpiiceDescriptor {
        spiice_id: SPIICE.into(),
        display_name: SPIICE.into(),
        package_version: "0".into(),
        author: None,
        provenance: SpiiceProvenance::Builtin,
    })?;
    r.try_add_fact_schema::<FactV2>()?;
    r.try_add_contract(&CONTRACT_V2)
}
fn register_v3(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
    r.try_add_spiice(SpiiceDescriptor {
        spiice_id: SPIICE.into(),
        display_name: SPIICE.into(),
        package_version: "0".into(),
        author: None,
        provenance: SpiiceProvenance::Builtin,
    })?;
    r.try_add_fact_schema::<FactV3>()?;
    r.try_add_contract(&CONTRACT_V3)
}
struct AppV1;
struct AppV2;
struct AppV3;
impl SpiiceBundle for AppV1 {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register_v1(r)
    }
    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_fact::<FactV1>();
    }
    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, migrator(evolve_retain))]
    }
}
impl SpiiceBundle for AppV2 {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register_v2(r)
    }
    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_fact::<FactV2>();
    }
    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, migrator(evolve_retain))]
    }
}
impl SpiiceBundle for AppV3 {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        r.try_add_spiice(SpiiceDescriptor {
            spiice_id: SPIICE.into(),
            display_name: SPIICE.into(),
            package_version: "0".into(),
            author: None,
            provenance: SpiiceProvenance::Builtin,
        })?;
        r.try_add_fact_schema::<FactV3>()?;
        r.try_add_contract(&CONTRACT_V3)
    }
    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_fact::<FactV3>();
    }
    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, chain_migrator())]
    }
}
struct AppV2Merge;
impl SpiiceBundle for AppV2Merge {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register_v2(r)
    }
    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_fact::<FactV2>();
    }
    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, migrator(evolve_merge))]
    }
}
struct AppV2Incomplete;
impl SpiiceBundle for AppV2Incomplete {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register_v2(r)
    }
    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_fact::<FactV2>();
    }
    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, migrator(evolve_incomplete))]
    }
}
struct AppV2Split;
impl SpiiceBundle for AppV2Split {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register_v2(r)
    }
    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_fact::<FactV2>();
    }
    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, migrator(evolve_split))]
    }
}
fn migrator(transform: proxiima::spiice::SchemaMigrationFn) -> Migrator {
    let mut registry = SpiiceRegistry::new();
    registry
        .try_add_spiice(SpiiceDescriptor {
            spiice_id: SPIICE.into(),
            display_name: SPIICE.into(),
            package_version: "0".into(),
            author: None,
            provenance: SpiiceProvenance::Builtin,
        })
        .unwrap();
    registry.try_add_fact_schema::<FactV1>().unwrap();
    registry.try_add_fact_schema::<FactV2>().unwrap();
    registry.try_add_contract(&CONTRACT_MIGRATOR).unwrap();
    let registry = registry.try_freeze().unwrap();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<FactV1>();
    sidecars.add_fact::<FactV2>();
    let mut migration_schemas = PgMigrationSchemas::new();
    migration_schemas.add_fact::<FactV1>();
    migration_schemas.add_fact::<FactV2>();
    sidecars.add_evolution(PgSchemaEvolution::new(
        &EVOLUTION,
        migration_schemas,
        transform,
    ));
    let sidecars = sidecars.freeze_against(&registry).unwrap();
    let mut sql = String::from(
        "CREATE SCHEMA IF NOT EXISTS evolution_test;
         CREATE TABLE IF NOT EXISTS evolution_test.fact_v1 (
             t uuid PRIMARY KEY REFERENCES proxiima_core.memory(t) ON DELETE CASCADE,
             task_id uuid NOT NULL, ended_at bigint NOT NULL, text text NOT NULL
         );
         CREATE TABLE IF NOT EXISTS evolution_test.fact_v2 (
             t uuid PRIMARY KEY REFERENCES proxiima_core.memory(t) ON DELETE CASCADE,
             task_id uuid NOT NULL, ended_at bigint NOT NULL, text text NOT NULL
         );
         INSERT INTO proxiima_core.spiice_surface (table_name, spiice_id)
             VALUES ('evolution_test.fact_v1', 'evolution-test'),
                    ('evolution_test.fact_v2', 'evolution-test')
             ON CONFLICT DO NOTHING;
         SELECT proxiima_core.install_owner_rls(
             'evolution_test', ARRAY[]::text[],
             ARRAY['fact_v1', 'fact_v2']::text[], ARRAY[]::text[]);",
    );
    for artifact in sidecars
        .declaration_trigger_artifacts(SPIICE)
        .unwrap()
        .into_iter()
        .chain(sidecars.presence_trigger_artifacts(SPIICE).unwrap())
    {
        // SQL-POLICY: generated — declaration and presence artifacts come from the frozen sidecar registry.
        sql.push_str(&artifact.forward);
        sql.push(';');
    }
    Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            ANCHOR,
            Cow::Borrowed("evolution sidecars"),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(sql).into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    }
}

fn chain_migrator() -> Migrator {
    let mut first = migrator(evolve_retain);
    first.migrations.to_mut().push(Migration::new(
        ANCHOR_V3,
        Cow::Borrowed("evolution v3 sidecars"),
        MigrationType::Simple,
        sqlx::AssertSqlSafe("CREATE TABLE IF NOT EXISTS evolution_test.fact_v3 (t uuid PRIMARY KEY REFERENCES proxiima_core.memory(t) ON DELETE CASCADE, task_id uuid NOT NULL, ended_at bigint NOT NULL, text text NOT NULL); INSERT INTO proxiima_core.spiice_surface (table_name, spiice_id) VALUES ('evolution_test.fact_v3', 'evolution-test') ON CONFLICT DO NOTHING; SELECT proxiima_core.install_owner_rls('evolution_test', ARRAY[]::text[], ARRAY['fact_v1', 'fact_v2', 'fact_v3']::text[], ARRAY[]::text[]);")
            .into_sql_str(),
        false,
    ));
    first.migrations.to_mut().push(Migration::new(
        CLEANUP_ANCHOR,
        Cow::Borrowed("remove obsolete evolution sidecars"),
        MigrationType::Simple,
        sqlx::AssertSqlSafe("DROP TABLE evolution_test.fact_v1; DROP TABLE evolution_test.fact_v2; DELETE FROM proxiima_core.spiice_surface WHERE table_name IN ('evolution_test.fact_v1', 'evolution_test.fact_v2');")
            .into_sql_str(),
        false,
    ));
    first
}
async fn storage(
    url: &str,
    platform: &str,
    registry: &proxiima_core::SpiiceRegistryFrozen,
    sidecars: PgSidecarRegistry,
) -> PgStorage {
    let pool = sqlx::PgPool::connect(platform).await.unwrap();
    let scope = PgPlatformScope::new(pool, &["proxiima_core", "evolution_test"])
        .await
        .unwrap();
    let storage = PgStorage::connect_with_config(
        url,
        PgPoolConfig {
            max_connections: 4,
            ..Default::default()
        },
        PgTuning::default(),
        scope,
        0,
    )
    .await
    .unwrap();
    storage
        .with_sidecars(sidecars.freeze_against(registry).unwrap())
        .await
        .unwrap()
        .with_spiices(registry)
}
fn sidecars_v1() -> PgSidecarRegistry {
    let mut s = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut s);
    AppV1::register_pg_sidecars(&mut s);
    s
}
fn sidecars_v2() -> PgSidecarRegistry {
    sidecars_v2_with(evolve_retain)
}
fn sidecars_v2_with(transform: proxiima::spiice::SchemaMigrationFn) -> PgSidecarRegistry {
    let mut s = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut s);
    AppV2::register_pg_sidecars(&mut s);
    let mut m = PgMigrationSchemas::new();
    m.add_fact::<FactV1>();
    m.add_fact::<FactV2>();
    s.add_evolution(PgSchemaEvolution::new(&EVOLUTION, m, transform));
    s
}
fn sidecars_v3() -> PgSidecarRegistry {
    sidecars_v3_with(evolve_retain, evolve_v2_to_v3)
}
fn sidecars_v3_with(
    first_transform: proxiima::spiice::SchemaMigrationFn,
    second_transform: proxiima::spiice::SchemaMigrationFn,
) -> PgSidecarRegistry {
    let mut s = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut s);
    AppV3::register_pg_sidecars(&mut s);
    let mut first = PgMigrationSchemas::new();
    first.add_fact::<FactV1>();
    first.add_fact::<FactV2>();
    s.add_evolution(PgSchemaEvolution::new(&EVOLUTION, first, first_transform));
    let mut second = PgMigrationSchemas::new();
    second.add_fact::<FactV2>();
    second.add_fact::<FactV3>();
    s.add_evolution(PgSchemaEvolution::new(
        &EVOLUTION_V2_TO_V3,
        second,
        second_transform,
    ));
    s
}
fn evolve_retain(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    for source in input.memories() {
        if let Some(old) = source.payload::<FactV1>() {
            plan.retain_fact(
                source,
                FactV2 {
                    task_id: old.task_id,
                    ended_at: old.ended_at,
                    text: old.text.clone(),
                },
            )?;
        }
    }
    Ok(())
}

fn evolve_v2_to_v3(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    for source in input.memories() {
        if let Some(old) = source.payload::<FactV2>() {
            plan.retain_fact(
                source,
                FactV3 {
                    task_id: old.task_id,
                    ended_at: old.ended_at,
                    text: old.text.clone(),
                },
            )?;
        }
    }
    Ok(())
}

fn evolve_merge(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    let sources = input
        .memories()
        .iter()
        .filter_map(|source| source.payload::<FactV1>().map(|payload| (source, payload)))
        .collect::<Vec<_>>();
    assert_eq!(
        sources.len(),
        2,
        "merge fixture must contain two source facts"
    );
    let (first, first_payload) = &sources[0];
    let target = plan.fact(
        first,
        FactV2 {
            task_id: first_payload.task_id,
            ended_at: first_payload.ended_at,
            text: format!("merged: {}", first_payload.text),
        },
        MigrationPlacement::NEW,
        MigrationPins::default(),
    )?;
    for (source, _) in &sources {
        plan.map_memory(source, target, &[])?;
    }
    Ok(())
}

fn evolve_incomplete(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    let source = input
        .memories()
        .iter()
        .find(|source| source.payload::<FactV1>().is_some())
        .expect("incomplete fixture must contain a source fact");
    let old = source.payload::<FactV1>().expect("FactV1 payload");
    let target = plan.fact(
        source,
        FactV2 {
            task_id: old.task_id,
            ended_at: old.ended_at,
            text: old.text.clone(),
        },
        MigrationPlacement::NEW,
        MigrationPins::default(),
    )?;
    plan.map_memory(source, target, &[])
}

fn evolve_split(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    let source = input
        .memories()
        .iter()
        .find(|source| source.payload::<FactV1>().is_some())
        .expect("split fixture must contain a source fact");
    let old = source.payload::<FactV1>().expect("FactV1 payload");
    let primary = plan.fact(
        source,
        FactV2 {
            task_id: old.task_id,
            ended_at: old.ended_at,
            text: "split-primary".into(),
        },
        MigrationPlacement::NEW,
        MigrationPins::default(),
    )?;
    let additional = plan.fact(
        source,
        FactV2 {
            task_id: Uuid::now_v7(),
            ended_at: old.ended_at + 1,
            text: "split-additional".into(),
        },
        MigrationPlacement::NEW,
        MigrationPins::default(),
    )?;
    plan.map_memory(source, primary, &[additional])
}

struct SeededFacts {
    db: proxiima_pg_testkit::SplitRoleDb,
    owner: OwnerId,
    ids: Vec<Uuid>,
    receipts: Vec<(Vec<u8>, Vec<u8>)>,
    ingest_keys: Vec<String>,
}

async fn seed_v1_facts(payloads: Vec<FactV1>) -> SeededFacts {
    let db = split_core_db::clone_split_core_db("proxiima_evolution_cases")
        .await
        .expect("PG required");
    let runtime_url = db.runtime_url().to_owned();
    let platform_url = db.platform_url().to_owned();
    let mut r1 = SpiiceRegistry::new();
    register_v1(&mut r1).unwrap();
    let r1 = r1.try_freeze().unwrap();
    let pg = PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let s1 = sidecars_v1();
    let f1 = s1.freeze_against(&r1).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppV1::migrators(), &f1)
        .await
        .unwrap();
    pg.initialize_store_number(0).await.unwrap();
    let old = storage(&runtime_url, &platform_url, &r1, sidecars_v1()).await;
    let old_engine = proxiima_core::Engine::new(r1)
        .try_with_publication_config(PublicationConfig::new(
            PublicationSource::new("urn:proxiima:evolution").unwrap(),
        ))
        .unwrap()
        .with_storage_ports(Arc::new(old).storage_ports());
    let owner = proxiima_core::OwnerId::new(Uuid::now_v7());
    let authz = authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        AuthPath::HostBearer,
    ));
    let admin = sqlx::PgPool::connect(&db.admin_url()).await.unwrap();
    sqlx::query("INSERT INTO proxiima_core.owners (owner_id, kind) VALUES ($1, 'personal'::proxiima_core.owner_kind) ON CONFLICT (owner_id) DO NOTHING")
        .bind(owner.into_inner())
        .execute(&admin)
        .await
        .unwrap();
    let mut ids = Vec::new();
    let mut receipts = Vec::new();
    let mut ingest_keys = Vec::new();
    for payload in payloads {
        let written = old_engine
            .ingest_fact(&authz, FactWrite::new(owner, SOURCE_FEED, &payload))
            .await
            .unwrap();
        let receipt: (Vec<u8>, Vec<u8>) = sqlx::query_as(
            "SELECT envelope, envelope_digest FROM proxiima_core.publication_outbox WHERE t=$1",
        )
        .bind(written.memory_id.into_inner())
        .fetch_one(&admin)
        .await
        .unwrap();
        ids.push(written.memory_id.into_inner());
        receipts.push(receipt);
        ingest_keys.push(
            sqlx::query_scalar(
                "SELECT ingest_key FROM proxiima_core.ingest_keys WHERE t=$1 AND source_id=$2",
            )
            .bind(written.memory_id.into_inner())
            .bind(SOURCE_FEED)
            .fetch_one(&admin)
            .await
            .unwrap(),
        );
    }
    SeededFacts {
        db,
        owner,
        ids,
        receipts,
        ingest_keys,
    }
}

async fn seed_two_v1_facts() -> SeededFacts {
    seed_v1_facts(vec![
        FactV1 {
            task_id: Uuid::now_v7(),
            ended_at: 7,
            text: "first".into(),
        },
        FactV1 {
            task_id: Uuid::now_v7(),
            ended_at: 8,
            text: "second".into(),
        },
    ])
    .await
}

#[tokio::test]
async fn fact_sidecar_rls_denies_cross_owner_reads() {
    let seeded = seed_v1_facts(vec![FactV1 {
        task_id: Uuid::now_v7(),
        ended_at: 7,
        text: "owner-only".into(),
    }])
    .await;
    let pool = sqlx::PgPool::connect(seeded.db.runtime_url())
        .await
        .unwrap();
    let foreign_owner = Uuid::now_v7();

    assert_eq!(visible_rows(&pool, seeded.owner.into_inner()).await, 1);
    assert_eq!(visible_rows(&pool, foreign_owner).await, 0);
}

async fn visible_rows(pool: &sqlx::PgPool, owner: Uuid) -> i64 {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "SELECT set_config('app.proxiima_scope', 'owner', true), set_config('app.owner', $1, true)",
    )
    .bind(format!("{{{owner}}}"))
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query_scalar("SELECT count(*) FROM evolution_test.fact_v1")
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}

async fn seed_goal_routes(
    seeded: &SeededFacts,
    permitted_use: &str,
    publish_both: bool,
) -> GoalHandle {
    let runtime_url = seeded.db.runtime_url().to_owned();
    let platform_url = seeded.db.platform_url().to_owned();
    let mut registry = SpiiceRegistry::new();
    register_v1(&mut registry).unwrap();
    let registry = registry.try_freeze().unwrap();
    let storage = storage(&runtime_url, &platform_url, &registry, sidecars_v1()).await;
    let engine =
        proxiima_core::Engine::new(registry).with_storage_ports(Arc::new(storage).storage_ports());
    let authz = authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(seeded.owner, Role::admin())],
        AuthPath::HostBearer,
    ));
    let mut unit = engine.unit_of_work(&authz).await.unwrap();
    let goal = unit
        .create_goal(GoalCreateRequest {
            owner: seeded.owner,
            topology: GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
            title: "merge conservation".into(),
            text: "goal route survives merge".into(),
            payload: SimpleTextGoalV1 {},
            request_id: IdempotencyKey::new("merge-conservation-goal").unwrap(),
            authorship: GoalAuthorship::User,
            author_self_perspective_id: None,
        })
        .await
        .unwrap();
    unit.commit().await.unwrap();
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    for (index, memory_t) in seeded.ids.iter().enumerate() {
        if !publish_both && index == 1 {
            continue;
        }
        let memory_handle: Uuid =
            sqlx::query_scalar("SELECT handle FROM proxiima_core.memory_head WHERE t=$1")
                .bind(memory_t)
                .fetch_one(&admin)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO proxiima_core.goal_memory_publication (goal_handle, memory_t, source_owner, source_generation, controller_generation, permitted_use, published_by) VALUES ($1,$2,$3,0,0,$4::proxiima_core.goal_publication_use,$3)",
        )
        .bind(goal.goal_handle.into_inner())
        .bind(memory_t)
        .bind(seeded.owner.into_inner())
        .bind(permitted_use)
        .execute(&admin)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO proxiima_core.goal_work_binding (goal_handle, memory_handle, source_owner, source_generation, controller_generation, bound_by) VALUES ($1,$2,$3,0,0,$3)",
        )
        .bind(goal.goal_handle.into_inner())
        .bind(memory_handle)
        .bind(seeded.owner.into_inner())
        .execute(&admin)
        .await
        .unwrap();
    }
    goal.goal_handle
}

async fn replay_fact_v2_and_check_receipt(
    engine: &proxiima_core::Engine,
    authz: &AuthzContext,
    owner: OwnerId,
    payload: FactV1,
    first: proxiima_core::MemoryId,
    receipt: (Vec<u8>, Vec<u8>, i64),
    admin: &sqlx::PgPool,
) {
    let replay = engine
        .ingest_fact(
            authz,
            FactWrite::new(
                owner,
                SOURCE_FEED,
                &FactV2 {
                    task_id: payload.task_id,
                    ended_at: payload.ended_at,
                    text: payload.text.clone(),
                },
            ),
        )
        .await
        .unwrap();
    assert_eq!(replay.memory_id, first);
    let after: (Vec<u8>, Vec<u8>, i64) = sqlx::query_as(
        "SELECT envelope, envelope_digest, count(*) OVER() FROM proxiima_core.publication_outbox WHERE t=$1",
    )
    .bind(first.into_inner())
    .fetch_one(admin)
    .await
    .unwrap();
    assert_eq!(after, receipt);
    let old_key = proxiima::spiice::FactWriteCommand::from_payload(SOURCE_FEED, &payload)
        .ingest_key
        .unwrap();
    let new_key = proxiima::spiice::FactWriteCommand::from_payload(
        SOURCE_FEED,
        &FactV2 {
            task_id: payload.task_id,
            ended_at: payload.ended_at,
            text: payload.text,
        },
    )
    .ingest_key
    .unwrap();
    assert_ne!(old_key, new_key);
    for key in [&old_key, &new_key] {
        let resolved: Uuid = sqlx::query_scalar(
            "SELECT t FROM proxiima_core.ingest_keys WHERE owner_id=$1 AND source_id=$2 AND ingest_key=$3",
        )
        .bind(owner.into_inner())
        .bind(SOURCE_FEED)
        .bind(key)
        .fetch_one(admin)
        .await
        .unwrap();
        assert_eq!(resolved, first.into_inner());
    }
}

#[tokio::test]
async fn fact_schema_evolution_preserves_receipt_and_replays_on_target() {
    let db = split_core_db::clone_split_core_db("proxiima_evolution")
        .await
        .expect("PG required");
    let runtime_url = db.runtime_url().to_owned();
    let platform_url = db.platform_url().to_owned();
    let mut r1 = SpiiceRegistry::new();
    register_v1(&mut r1).unwrap();
    let r1 = r1.try_freeze().unwrap();
    let pg = PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let s1 = sidecars_v1();
    let f1 = s1.freeze_against(&r1).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppV1::migrators(), &f1)
        .await
        .unwrap();
    pg.initialize_store_number(0).await.unwrap();
    let old = storage(&runtime_url, &platform_url, &r1, sidecars_v1()).await;
    let source = PublicationSource::new("urn:proxiima:evolution").unwrap();
    let old_engine = proxiima_core::Engine::new(r1.clone())
        .try_with_publication_config(PublicationConfig::new(source))
        .unwrap()
        .with_storage_ports(Arc::new(old).storage_ports());
    let owner = proxiima_core::OwnerId::new(Uuid::now_v7());
    let authz = authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(owner, Role::admin())],
        AuthPath::HostBearer,
    ));
    let admin = sqlx::PgPool::connect(&db.admin_url()).await.unwrap();
    sqlx::query("INSERT INTO proxiima_core.owners (owner_id, kind) VALUES ($1, 'personal'::proxiima_core.owner_kind) ON CONFLICT (owner_id) DO NOTHING")
        .bind(owner.into_inner()).execute(&admin).await.unwrap();
    let payload = FactV1 {
        task_id: Uuid::now_v7(),
        ended_at: 7,
        text: "finished".into(),
    };
    let first = old_engine
        .ingest_fact(&authz, FactWrite::new(owner, SOURCE_FEED, &payload))
        .await
        .unwrap();
    let receipt: (Vec<u8>, Vec<u8>, i64) = sqlx::query_as(
        "SELECT envelope, envelope_digest, count(*) OVER() FROM proxiima_core.publication_outbox WHERE t=$1",
    )
    .bind(first.memory_id.into_inner())
    .fetch_one(&admin)
    .await
    .unwrap();
    let mut r2 = SpiiceRegistry::new();
    register_v2(&mut r2).unwrap();
    let r2 = r2.try_freeze().unwrap();
    let s2 = sidecars_v2();
    let f2 = s2.clone().freeze_against(&r2).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppV2::migrators(), &f2)
        .await
        .unwrap();
    let new = storage(&runtime_url, &platform_url, &r2, s2).await;
    let new_engine = proxiima_core::Engine::new(r2)
        .try_with_publication_config(PublicationConfig::new(
            PublicationSource::new("urn:proxiima:evolution").unwrap(),
        ))
        .unwrap()
        .with_storage_ports(Arc::new(new).storage_ports());
    replay_fact_v2_and_check_receipt(
        &new_engine,
        &authz,
        owner,
        payload,
        first.memory_id,
        receipt,
        &admin,
    )
    .await;
}

#[tokio::test]
async fn fact_schema_evolution_merges_two_sources_into_one_new_target() {
    let seeded = seed_two_v1_facts().await;
    let goal = seed_goal_routes(&seeded, "read", true).await;
    let platform_url = seeded.db.platform_url().to_owned();
    let pg = PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let mut r2 = SpiiceRegistry::new();
    register_v2(&mut r2).unwrap();
    let r2 = r2.try_freeze().unwrap();
    let s2 = sidecars_v2_with(evolve_merge);
    let f2 = s2.clone().freeze_against(&r2).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppV2Merge::migrators(), &f2)
        .await
        .unwrap();

    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    assert_fact_merge(&seeded, goal, admin).await;
}

async fn assert_fact_merge(seeded: &SeededFacts, goal: GoalHandle, admin: sqlx::PgPool) {
    let target_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxiima_core.memory WHERE schema_id=$1 ORDER BY t")
            .bind(TARGET_ID)
            .fetch_all(&admin)
            .await
            .unwrap();
    assert_eq!(target_ids.len(), 1);
    let target = target_ids[0];
    assert!(seeded.ids.iter().all(|id| *id != target));
    let target_handle: Uuid =
        sqlx::query_scalar("SELECT handle FROM proxiima_core.memory_head WHERE t=$1")
            .bind(target)
            .fetch_one(&admin)
            .await
            .unwrap();
    let publications: Vec<(Uuid, Uuid, i64, i64, String)> = sqlx::query_as(
        "SELECT goal_handle, memory_t, source_generation, controller_generation, permitted_use::text FROM proxiima_core.goal_memory_publication WHERE goal_handle=$1",
    )
    .bind(goal.into_inner())
    .fetch_all(&admin)
    .await
    .unwrap();
    assert_eq!(publications.len(), 1);
    assert_eq!(publications[0].1, target);
    assert_eq!(publications[0].2, 0);
    assert_eq!(publications[0].3, 0);
    assert_eq!(publications[0].4, "read");
    let bindings: Vec<(Uuid, Uuid, i64, i64)> = sqlx::query_as(
        "SELECT goal_handle, memory_handle, source_generation, controller_generation FROM proxiima_core.goal_work_binding WHERE goal_handle=$1",
    )
    .bind(goal.into_inner())
    .fetch_all(&admin)
    .await
    .unwrap();
    assert_eq!(bindings, vec![(goal.into_inner(), target_handle, 0, 0)]);

    for old_id in &seeded.ids {
        let memory_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM proxiima_core.memory WHERE t=$1")
                .bind(old_id)
                .fetch_one(&admin)
                .await
                .unwrap();
        let sidecar_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM evolution_test.fact_v1 WHERE t=$1")
                .bind(old_id)
                .fetch_one(&admin)
                .await
                .unwrap();
        assert_eq!(memory_count, 0, "source memory removed");
        assert_eq!(sidecar_count, 0, "source sidecar removed");
    }
    let target_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxiima_core.memory WHERE t=$1 AND schema_id=$2")
            .bind(target)
            .bind(TARGET_ID)
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(target_count, 1);

    for key in &seeded.ingest_keys {
        let resolved: Uuid = sqlx::query_scalar(
            "SELECT t FROM proxiima_core.ingest_keys WHERE source_id=$1 AND ingest_key=$2",
        )
        .bind(SOURCE_FEED)
        .bind(key)
        .fetch_one(&admin)
        .await
        .unwrap();
        assert_eq!(
            resolved, target,
            "old receipt alias resolves to merged target"
        );
    }
    let aliases: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT publication_t, memory_t FROM proxiima_core.schema_evolution_publication WHERE memory_t=$1 ORDER BY publication_t",
    )
    .bind(target)
    .fetch_all(&admin)
    .await
    .unwrap();
    assert_eq!(
        aliases,
        seeded
            .ids
            .iter()
            .map(|id| (*id, target))
            .collect::<Vec<_>>()
    );

    assert_merge_receipts(seeded, admin).await;
}

async fn assert_merge_receipts(seeded: &SeededFacts, admin: sqlx::PgPool) {
    let outbox: Vec<(Uuid, Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT t, envelope, envelope_digest FROM proxiima_core.publication_outbox ORDER BY t",
    )
    .fetch_all(&admin)
    .await
    .unwrap();
    assert_eq!(
        outbox.len(),
        seeded.ids.len(),
        "merge creates no publication"
    );
    let mut actual_receipts: Vec<_> = outbox
        .into_iter()
        .map(|(_, envelope, digest)| (envelope, digest))
        .collect();
    let mut expected_receipts = seeded.receipts.clone();
    actual_receipts.sort();
    expected_receipts.sort();
    assert_eq!(actual_receipts, expected_receipts);
}

#[tokio::test]
async fn incomplete_goal_collaboration_blocks_fact_merge_without_changes() {
    let seeded = seed_two_v1_facts().await;
    let goal = seed_goal_routes(&seeded, "read", false).await;
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    let before_publications: Vec<(Uuid, Uuid, i64, i64, String)> = sqlx::query_as(
        "SELECT goal_handle, memory_t, source_generation, controller_generation, permitted_use::text FROM proxiima_core.goal_memory_publication WHERE goal_handle=$1 ORDER BY memory_t",
    )
    .bind(goal.into_inner())
    .fetch_all(&admin)
    .await
    .unwrap();
    let before_bindings: Vec<(Uuid, Uuid, i64, i64)> = sqlx::query_as(
        "SELECT goal_handle, memory_handle, source_generation, controller_generation FROM proxiima_core.goal_work_binding WHERE goal_handle=$1 ORDER BY memory_handle",
    )
    .bind(goal.into_inner())
    .fetch_all(&admin)
    .await
    .unwrap();
    let platform_url = seeded.db.platform_url().to_owned();
    let pg = PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let mut r2 = SpiiceRegistry::new();
    register_v2(&mut r2).unwrap();
    let r2 = r2.try_freeze().unwrap();
    let s2 = sidecars_v2_with(evolve_merge);
    let f2 = s2.clone().freeze_against(&r2).unwrap();
    let error = proxiima::run_core_and_spiice_migrations(&pg, AppV2Merge::migrators(), &f2)
        .await
        .expect_err("a partially published Goal must refuse the merge");
    assert!(
        error.to_string().contains("collaboration") || error.to_string().contains("merge"),
        "{error}"
    );
    let after_publications: Vec<(Uuid, Uuid, i64, i64, String)> = sqlx::query_as(
        "SELECT goal_handle, memory_t, source_generation, controller_generation, permitted_use::text FROM proxiima_core.goal_memory_publication WHERE goal_handle=$1 ORDER BY memory_t",
    )
    .bind(goal.into_inner())
    .fetch_all(&admin)
    .await
    .unwrap();
    let after_bindings: Vec<(Uuid, Uuid, i64, i64)> = sqlx::query_as(
        "SELECT goal_handle, memory_handle, source_generation, controller_generation FROM proxiima_core.goal_work_binding WHERE goal_handle=$1 ORDER BY memory_handle",
    )
    .bind(goal.into_inner())
    .fetch_all(&admin)
    .await
    .unwrap();
    assert_eq!(after_publications, before_publications);
    assert_eq!(after_bindings, before_bindings);
}

#[tokio::test]
async fn incomplete_fact_schema_evolution_rolls_back_everything() {
    let seeded = seed_two_v1_facts().await;
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    let before_memories: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT t, schema_id FROM proxiima_core.memory ORDER BY t")
            .fetch_all(&admin)
            .await
            .unwrap();
    let before_sidecars: Vec<(Uuid, Uuid, i64, String)> =
        sqlx::query_as("SELECT t, task_id, ended_at, text FROM evolution_test.fact_v1 ORDER BY t")
            .fetch_all(&admin)
            .await
            .unwrap();
    let before_outbox: Vec<(Uuid, Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT t, envelope, envelope_digest FROM proxiima_core.publication_outbox ORDER BY t",
    )
    .fetch_all(&admin)
    .await
    .unwrap();
    let before_aliases: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT publication_t, memory_t FROM proxiima_core.schema_evolution_publication ORDER BY publication_t, memory_t",
    )
    .fetch_all(&admin)
    .await
    .unwrap();

    let platform_url = seeded.db.platform_url().to_owned();
    let pg = PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let mut r2 = SpiiceRegistry::new();
    register_v2(&mut r2).unwrap();
    let r2 = r2.try_freeze().unwrap();
    let s2 = sidecars_v2_with(evolve_incomplete);
    let f2 = s2.clone().freeze_against(&r2).unwrap();
    let error = proxiima::run_core_and_spiice_migrations(&pg, AppV2Incomplete::migrators(), &f2)
        .await
        .expect_err("an omitted source mapping must abort the migration");
    assert!(matches!(
        error,
        proxiima::MigrationError::SchemaEvolution(_)
    ));
    assert!(
        error
            .to_string()
            .contains("migration mappings do not cover exactly"),
        "{error}"
    );

    let after_memories: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT t, schema_id FROM proxiima_core.memory ORDER BY t")
            .fetch_all(&admin)
            .await
            .unwrap();
    let after_sidecars: Vec<(Uuid, Uuid, i64, String)> =
        sqlx::query_as("SELECT t, task_id, ended_at, text FROM evolution_test.fact_v1 ORDER BY t")
            .fetch_all(&admin)
            .await
            .unwrap();
    let after_outbox: Vec<(Uuid, Vec<u8>, Vec<u8>)> = sqlx::query_as(
        "SELECT t, envelope, envelope_digest FROM proxiima_core.publication_outbox ORDER BY t",
    )
    .fetch_all(&admin)
    .await
    .unwrap();
    let after_aliases: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT publication_t, memory_t FROM proxiima_core.schema_evolution_publication ORDER BY publication_t, memory_t",
    )
    .fetch_all(&admin)
    .await
    .unwrap();
    let completions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxiima_core.schema_evolution WHERE evolution_id=$1",
    )
    .bind("fact-v1-to-v2")
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(after_memories, before_memories);
    assert_eq!(after_sidecars, before_sidecars);
    assert_eq!(after_outbox, before_outbox);
    assert_eq!(after_aliases, before_aliases);
    assert_eq!(completions, 0);
}

#[tokio::test]
async fn fact_schema_evolution_splits_one_source_and_keeps_one_publication() {
    let seeded = seed_v1_facts(vec![FactV1 {
        task_id: Uuid::now_v7(),
        ended_at: 7,
        text: "split-source".into(),
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
    let mut r2 = SpiiceRegistry::new();
    register_v2(&mut r2).unwrap();
    let r2 = r2.try_freeze().unwrap();
    let s2 = sidecars_v2_with(evolve_split);
    let f2 = s2.clone().freeze_against(&r2).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppV2Split::migrators(), &f2)
        .await
        .unwrap();

    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    let target_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT t FROM proxiima_core.memory WHERE schema_id=$1 ORDER BY t")
            .bind(TARGET_ID)
            .fetch_all(&admin)
            .await
            .unwrap();
    assert_eq!(target_ids.len(), 2);
    assert!(seeded.ids.iter().all(|id| !target_ids.contains(id)));
    let aliases: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxiima_core.schema_evolution_publication WHERE memory_t=ANY($1)",
    )
    .bind(&target_ids)
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(
        aliases, 2,
        "one original publication links to both split targets"
    );
    let outbox: (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT envelope, envelope_digest FROM proxiima_core.publication_outbox")
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(outbox, seeded.receipts[0]);
    let outbox_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxiima_core.publication_outbox")
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(outbox_count, 1, "split creates no extra publication event");

    let s1 = sidecars_v1();
    let r1 = {
        let mut registry = SpiiceRegistry::new();
        register_v1(&mut registry).unwrap();
        registry.try_freeze().unwrap()
    };
    let f1 = s1.freeze_against(&r1).unwrap();
    let mut connection = admin.acquire().await.unwrap();
    assert!(f1.ensure_schema_evolutions(&mut connection).await.is_err());
    drop(connection);
    proxiima::run_core_and_spiice_migrations(&pg, AppV2Split::migrators(), &f2)
        .await
        .unwrap();
    let outbox_count_after_retry: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxiima_core.publication_outbox")
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(outbox_count_after_retry, 1);
    erasure::assert_split_capture_erasure(&seeded, &target_ids).await;
}

#[tokio::test]
async fn fact_schema_evolution_runs_v1_to_v3_chain_with_only_v3_active() {
    let source = FactV1 {
        task_id: Uuid::now_v7(),
        ended_at: 9,
        text: "chain-source".into(),
    };
    let seeded = seed_v1_facts(vec![source.clone()]).await;
    let platform_url = seeded.db.platform_url().to_owned();
    let pg = PgMigrationStorage::connect_with_config(
        &platform_url,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let mut r3 = SpiiceRegistry::new();
    register_v3(&mut r3).unwrap();
    let r3 = r3.try_freeze().unwrap();
    let s3 = sidecars_v3();
    let f3 = s3.clone().freeze_against(&r3).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppV3::migrators(), &f3)
        .await
        .unwrap();

    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    let counts: (Option<String>, Option<String>, i64) = sqlx::query_as(
        "SELECT to_regclass('evolution_test.fact_v1')::text, to_regclass('evolution_test.fact_v2')::text, (SELECT count(*) FROM evolution_test.fact_v3)",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(
        counts,
        (None, None, 1),
        "only the active v3 sidecar remains"
    );
    let completed: Vec<String> = sqlx::query_scalar(
        "SELECT evolution_id FROM proxiima_core.schema_evolution ORDER BY evolution_id",
    )
    .fetch_all(&admin)
    .await
    .unwrap();
    assert_eq!(completed, vec!["fact-v1-to-v2", "fact-v2-to-v3"]);

    let old_receipt: (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT envelope, envelope_digest FROM proxiima_core.publication_outbox WHERE t=$1",
    )
    .bind(seeded.ids[0])
    .fetch_one(&admin)
    .await
    .unwrap();
    let active = storage(seeded.db.runtime_url(), &platform_url, &r3, s3).await;
    let engine = proxiima_core::Engine::new(r3)
        .try_with_publication_config(PublicationConfig::new(
            PublicationSource::new("urn:proxiima:evolution").unwrap(),
        ))
        .unwrap()
        .with_storage_ports(Arc::new(active).storage_ports());
    let owner: Uuid = sqlx::query_scalar("SELECT owner_id FROM proxiima_core.memory WHERE t=$1")
        .bind(seeded.ids[0])
        .fetch_one(&admin)
        .await
        .unwrap();
    let authz = authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(proxiima_core::OwnerId::new(owner), Role::admin())],
        AuthPath::HostBearer,
    ));
    let replay = engine
        .ingest_fact(
            &authz,
            FactWrite::new(
                proxiima_core::OwnerId::new(owner),
                SOURCE_FEED,
                &FactV3 {
                    task_id: source.task_id,
                    ended_at: source.ended_at,
                    text: source.text,
                },
            ),
        )
        .await
        .unwrap();
    assert_eq!(replay.memory_id.into_inner(), seeded.ids[0]);
    let new_receipt: (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT envelope, envelope_digest FROM proxiima_core.publication_outbox WHERE t=$1",
    )
    .bind(seeded.ids[0])
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(new_receipt, old_receipt, "publication bytes stay immutable");
}

fn evolve_new_v1_to_v2(
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

fn evolve_new_v2_to_v3(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    for source in input.memories() {
        if let Some(old) = source.payload::<FactV2>() {
            let target = plan.fact(
                source,
                FactV3 {
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

async fn run_replacement_chain(second_transform: proxiima::spiice::SchemaMigrationFn) {
    let source = FactV1 {
        task_id: Uuid::now_v7(),
        ended_at: 19,
        text: "replacement-chain".into(),
    };
    let seeded = seed_v1_facts(vec![source.clone()]).await;
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    let receipt_before: (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT envelope, envelope_digest FROM proxiima_core.publication_outbox WHERE t=$1",
    )
    .bind(seeded.ids[0])
    .fetch_one(&admin)
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
    register_v3(&mut registry).unwrap();
    let registry = registry.try_freeze().unwrap();
    let sidecars = sidecars_v3_with(evolve_new_v1_to_v2, second_transform);
    let frozen = sidecars.clone().freeze_against(&registry).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppV3::migrators(), &frozen)
        .await
        .unwrap();
    let final_id = assert_terminal_chain(&admin, &seeded, receipt_before).await;
    let active = storage(
        seeded.db.runtime_url(),
        seeded.db.platform_url(),
        &registry,
        sidecars,
    )
    .await;
    let engine = proxiima_core::Engine::new(registry)
        .try_with_publication_config(PublicationConfig::new(
            PublicationSource::new("urn:proxiima:evolution").unwrap(),
        ))
        .unwrap()
        .with_storage_ports(Arc::new(active).storage_ports());
    let authz = authenticated_context(AuthzContext::for_subject_with_role(
        UserId::new(Uuid::now_v7()),
        [(seeded.owner, Role::admin())],
        AuthPath::HostBearer,
    ));
    let replay = engine
        .ingest_fact(
            &authz,
            FactWrite::new(
                seeded.owner,
                SOURCE_FEED,
                &FactV3 {
                    task_id: source.task_id,
                    ended_at: source.ended_at,
                    text: source.text,
                },
            ),
        )
        .await
        .unwrap();
    assert_eq!(replay.memory_id.into_inner(), final_id);
}

async fn assert_terminal_chain(
    admin: &sqlx::PgPool,
    seeded: &SeededFacts,
    receipt_before: (Vec<u8>, Vec<u8>),
) -> Uuid {
    let final_id: Uuid = sqlx::query_scalar(
        "SELECT t FROM proxiima_core.memory WHERE owner_id=$1 AND kind='fact' AND schema_id=$2",
    )
    .bind(seeded.owner.into_inner())
    .bind(FactV3::SCHEMA_ID)
    .fetch_one(admin)
    .await
    .unwrap();
    assert_ne!(final_id, seeded.ids[0]);
    let witness_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxiima_core.erased_pin_target")
            .fetch_one(admin)
            .await
            .unwrap();
    assert_eq!(
        witness_count, 0,
        "transitive replacement must not leave erased witnesses"
    );
    let receipt_after: (Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT envelope, envelope_digest FROM proxiima_core.publication_outbox WHERE t=$1",
    )
    .bind(seeded.ids[0])
    .fetch_one(admin)
    .await
    .unwrap();
    assert_eq!(receipt_after, receipt_before);
    let authority_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxiima_core.schema_evolution_authority")
            .fetch_one(admin)
            .await
            .unwrap();
    assert_eq!(authority_count, 0);
    let replayed_by_old_key: Uuid = sqlx::query_scalar(
        "SELECT t FROM proxiima_core.ingest_keys WHERE owner_id=$1 AND source_id=$2 AND ingest_key=$3",
    )
    .bind(seeded.owner.into_inner())
    .bind(SOURCE_FEED)
    .bind(&seeded.ingest_keys[0])
    .fetch_one(admin)
    .await
    .unwrap();
    assert_eq!(
        replayed_by_old_key, final_id,
        "the original admission key follows the replacement chain"
    );
    final_id
}

#[tokio::test]
async fn fact_schema_evolution_new_then_retain_preserves_publication_capture() {
    run_replacement_chain(evolve_v2_to_v3).await;
}

#[tokio::test]
async fn fact_schema_evolution_new_then_new_preserves_publication_capture() {
    run_replacement_chain(evolve_new_v2_to_v3).await;
}

#[tokio::test]
async fn fact_schema_evolution_chain_skips_completed_first_step_and_is_idempotent() {
    let seeded = seed_v1_facts(vec![FactV1 {
        task_id: Uuid::now_v7(),
        ended_at: 10,
        text: "partial-chain".into(),
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
    let mut r2 = SpiiceRegistry::new();
    register_v2(&mut r2).unwrap();
    let r2 = r2.try_freeze().unwrap();
    let s2 = sidecars_v2();
    let f2 = s2.clone().freeze_against(&r2).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppV2::migrators(), &f2)
        .await
        .unwrap();

    let mut r3 = SpiiceRegistry::new();
    register_v3(&mut r3).unwrap();
    let r3 = r3.try_freeze().unwrap();
    let s3 = sidecars_v3();
    let f3 = s3.clone().freeze_against(&r3).unwrap();
    let admin = sqlx::PgPool::connect(&seeded.db.admin_url()).await.unwrap();
    let mut connection = admin.acquire().await.unwrap();
    let pending = f3.pending_schema_evolutions(&mut connection).await.unwrap();
    assert_eq!(
        pending.iter().map(|step| step.id).collect::<Vec<_>>(),
        ["fact-v2-to-v3"]
    );
    drop(connection);
    proxiima::run_core_and_spiice_migrations(&pg, AppV3::migrators(), &f3)
        .await
        .unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppV3::migrators(), &f3)
        .await
        .unwrap();
    let completed: Vec<String> = sqlx::query_scalar(
        "SELECT evolution_id FROM proxiima_core.schema_evolution ORDER BY evolution_id",
    )
    .fetch_all(&admin)
    .await
    .unwrap();
    assert_eq!(completed, vec!["fact-v1-to-v2", "fact-v2-to-v3"]);
    let counts: (Option<String>, Option<String>, i64) = sqlx::query_as(
        "SELECT to_regclass('evolution_test.fact_v1')::text, to_regclass('evolution_test.fact_v2')::text, (SELECT count(*) FROM evolution_test.fact_v3)",
    )
    .fetch_one(&admin)
    .await
    .unwrap();
    assert_eq!(counts, (None, None, 1));
}
