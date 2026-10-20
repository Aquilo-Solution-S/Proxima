//! A real stateful tombstone payload, isolated from the example note schema.

use std::sync::Arc;

use proxima::flavor::{
    CounterRule, DbConstraint, EmbeddingRecipe, EraseRule, ExportRule, FactPayload, FactTombstone,
    FlavorBundle, FlavorContract, FlavorRegistry, ForgetRule, KeyShape, PayloadKeyBuilder,
    PayloadKind, PgSidecarRegistry, ProjectionDecl, Provenance, SchemaContract, SchemaRef,
    SearchProjectionDecl, SidecarHeadsScope, Surface, TransferRule, authorized_fact_payloads,
    sidecar_heads_by_key,
};
use proxima::{FactWrite, MemoryId};
use proxima_example::ExampleFlavor;
use proxima_storage_pg::{PgMigrationStorage, PgPlatformScope, PgPoolConfig, PgStorage, PgTuning};
use serde::{Deserialize, Serialize};

use super::{TestResult, authz, example_engine, owner};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HeadObservation {
    key: String,
    state: String,
}

fn observation(key: &str, state: &str) -> HeadObservation {
    HeadObservation {
        key: key.to_owned(),
        state: state.to_owned(),
    }
}

impl FactPayload for HeadObservation {
    const SCHEMA_ID: &'static str = "heads-fixture/observation-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("key", &self.key);
        key.field_str("state", &self.state);
        key.finish()
    }

    fn render(&self) -> String {
        self.state.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("heads_fixture.observation_v1")
    }

    fn natural_key_columns() -> &'static [&'static str] {
        &["key"]
    }

    fn tombstone() -> Option<FactTombstone> {
        Some(FactTombstone {
            column: "state",
            value: "deleted",
        })
    }
}

static CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "heads-fixture",
    ordinal: 79,
    schemas: &[SchemaContract {
        id: SchemaRef::new("heads-fixture", "observation", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("heads_fixture.observation_v1"),
        search: SearchProjectionDecl::None {
            why: "head listing uses the declared natural key",
        },
        embedding: EmbeddingRecipe::Never {
            why: "the fixture carries state observations",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[Surface {
            table: "heads_fixture.observation_v1",
            key: KeyShape::MemoryT {
                column: "admission_id",
            },
            owner_column: None,
            transfer: TransferRule::StaysOnKey,
            erase: EraseRule::ByKey,
            export: ExportRule::Rows,
            forget: ForgetRule::DumpThenDelete,
            lexical_language_column: None,
            counter: CounterRule::Counted("sidecar_rows"),
            completeness: Some(DbConstraint {
                relation: "heads_fixture.observation_v1",
                name: "observation_v1_admission_id_fkey",
            }),
        }],
        natural_key_columns: &["key"],
    }],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None {
        why: "the fixture is read by natural key",
    },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

proxima_core::proxima_flavor! {
    name = "heads-fixture",
    fact_schemas = [HeadObservation],
    contract = &CONTRACT,
}

proxima::flavor::pg_sidecar! {
    payload: HeadObservation,
    row: ObservationRow,
    kinds: [Fact],
    table: "heads_fixture.observation_v1",
    key: admission_id,
    fields: {
        key => key: (text),
        state => state: (text),
    },
}

pub async fn assert_tombstone_head() -> TestResult {
    let db = example_engine::migrated_db("sidecar_heads_tombstone").await;
    PgMigrationStorage::connect_with_config(
        db.platform_url(),
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await?
    .initialize_store_number(0)
    .await?;
    let mut registry = FlavorRegistry::new();
    ExampleFlavor::register(&mut registry)?;
    register(&mut registry)?;
    let registry = registry.try_freeze()?;
    let mut sidecars = PgSidecarRegistry::new();
    proxima::flavor::register_core_pg_sidecars(&mut sidecars);
    ExampleFlavor::register_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<HeadObservation>();
    let sidecars = sidecars.freeze_against(&registry)?;
    let admin = sqlx::PgPool::connect(&db.admin_url()).await?;
    sqlx::raw_sql(
        "CREATE SCHEMA heads_fixture;
         CREATE TABLE heads_fixture.observation_v1 (
             admission_id uuid PRIMARY KEY REFERENCES proxima_core.memory(t),
             key text NOT NULL, state text NOT NULL
         );
         ALTER SCHEMA heads_fixture OWNER TO proxima_test_platform;
         ALTER TABLE heads_fixture.observation_v1 OWNER TO proxima_test_platform;
         INSERT INTO proxima_core.flavor_surface(table_name,flavor_id)
             VALUES ('heads_fixture.observation_v1','heads-fixture');
         SELECT proxima_core.install_owner_rls('heads_fixture', ARRAY[]::text[],
             ARRAY['observation_v1']::text[], ARRAY[]::text[]);
         GRANT USAGE ON SCHEMA heads_fixture TO proxima_test_runtime;
         GRANT SELECT, INSERT, UPDATE, DELETE ON heads_fixture.observation_v1 TO proxima_test_runtime;",
    ).execute(&admin).await?;
    let mut triggers = sidecars.declaration_trigger_artifacts("heads-fixture")?;
    triggers.extend(sidecars.presence_trigger_artifacts("heads-fixture")?);
    for artifact in triggers {
        // SQL-POLICY: generated — the frozen fixture registry owns the trigger identifiers.
        sqlx::raw_sql(sqlx::AssertSqlSafe(artifact.forward))
            .execute(&admin)
            .await?;
    }
    let platform = sqlx::PgPool::connect(db.platform_url()).await?;
    let platform = PgPlatformScope::new(
        platform,
        &["proxima_core", "proxima_example", "heads_fixture"],
    )
    .await?;
    let storage = PgStorage::connect(db.runtime_url(), platform, 0)
        .await?
        .with_sidecars(sidecars)
        .with_flavors(&registry);
    let engine =
        proxima::Engine::new(registry).with_storage_ports(Arc::new(storage).storage_ports());
    let owner = owner();
    let authz = authz(&[owner]);
    let mut admitted = Vec::<MemoryId>::new();
    for payload in [
        observation("retired", "live"),
        observation("retired", "deleted"),
        observation("other", "live"),
    ] {
        admitted.push(
            engine
                .ingest_fact(&authz, FactWrite::new(owner, "head-fixture", &payload))
                .await?
                .memory_id,
        );
    }
    let page = sidecar_heads_by_key::<HeadObservation>(
        &engine,
        &authz,
        SidecarHeadsScope::Owner(owner),
        &[],
        None,
        100,
    )
    .await?;
    let ids: std::collections::BTreeSet<_> = page.heads.iter().map(|head| head.memory_id).collect();
    assert_eq!(
        ids,
        std::collections::BTreeSet::from([admitted[1], admitted[2]])
    );
    assert!(!ids.contains(&admitted[0]));
    let payloads = authorized_fact_payloads::<HeadObservation>(
        &engine,
        &authz,
        &[admitted[1].into_inner()],
        1,
    )
    .await?;
    assert_eq!(payloads.len(), 1);
    assert_eq!(
        payloads[0].1.state, "deleted",
        "the deletion observation remains a hot head"
    );
    admin.close().await;
    Ok(())
}
