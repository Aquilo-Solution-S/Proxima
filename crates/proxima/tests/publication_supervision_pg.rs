use std::borrow::Cow;
use std::sync::Arc;

use futures::FutureExt;
use proxima::flavor::{
    FactPayload, FlavorBundle, FlavorDescriptor, FlavorProvenance, FlavorRegistry,
    FlavorRegistryError,
};
use proxima::{AppInfo, FlavorApp, Proxima, RuntimeBuilder, ToolScope, company_owner};
use proxima_core::flavor::{
    EmbeddingRecipe, FlavorContract, ProjectionDecl, Provenance, SchemaContract, SchemaRef,
    SearchProjectionDecl, TransferRule,
};
use proxima_core::publication::{PublicationConfig, PublicationSource};
use proxima_core::test_fixtures::authenticated_context;
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{AuthzContext, Engine, Owner};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use uuid::Uuid;

const PROBE_MIGRATION_VERSION: i64 = 20_260_920_000_012;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct HealthProbeV1 {
    probe_id: Uuid,
    note: String,
}

impl FactPayload for HealthProbeV1 {
    const SCHEMA_ID: &'static str = "publisher-health/probe-v1";
    const SCHEMA_VERSION: u32 = 1;
    const LISTENABLE: bool = true;

    fn receipt_key(&self) -> Vec<u8> {
        self.probe_id.as_bytes().to_vec()
    }

    fn render(&self) -> String {
        self.note.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("publisher_health.probe_v1")
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "required": ["probe_id", "note"],
            "properties": {
                "probe_id": { "type": "string", "format": "uuid" },
                "note": { "type": "string" }
            }
        }))
    }
}

proxima::flavor::pg_sidecar! {
    payload: HealthProbeV1,
    row: PublisherHealthProbeRow,
    kinds: [Fact],
    table: "publisher_health.probe_v1",
    key: t,
    fields: {
        probe_id => probe_id: (uuid),
        note => note: (text),
    },
}

static HEALTH_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "publisher-health",
    ordinal: 19,
    schemas: &[SchemaContract {
        id: SchemaRef::new("publisher-health", "probe", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("publisher_health.probe_v1"),
        search: SearchProjectionDecl::None {
            why: "a publisher supervision fixture, not a search surface",
        },
        embedding: EmbeddingRecipe::Never {
            why: "a publisher supervision fixture, not a memory",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
    }],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::None {
        why: "a publisher supervision fixture has no projection",
    },
};

fn probe_migrator() -> Migrator {
    let mut registry = FlavorRegistry::new();
    PublisherApp::register(&mut registry).expect("publisher fixture registers");
    let registry = registry.try_freeze().expect("publisher fixture freezes");
    let mut sidecars = proxima::flavor::PgSidecarRegistry::new();
    proxima_storage_pg::register_core_pg_sidecars(&mut sidecars);
    PublisherApp::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars
        .freeze_against(&registry)
        .expect("publisher fixture PG sidecar matches its contract");

    let mut statements = vec![
        "CREATE SCHEMA publisher_health".to_owned(),
        "CREATE TABLE publisher_health.probe_v1 (
            t uuid PRIMARY KEY,
            probe_id uuid NOT NULL,
            note text NOT NULL
        )"
        .to_owned(),
        "ALTER TABLE publisher_health.probe_v1 ENABLE ROW LEVEL SECURITY".to_owned(),
        "ALTER TABLE publisher_health.probe_v1 FORCE ROW LEVEL SECURITY".to_owned(),
        "CREATE POLICY proxima_owner_read ON publisher_health.probe_v1 FOR SELECT TO PUBLIC USING (EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = probe_v1.t AND parent.owner_id = ANY(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))))".to_owned(),
        "CREATE POLICY proxima_owner_write ON publisher_health.probe_v1 FOR ALL TO PUBLIC USING (EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = probe_v1.t AND parent.owner_id = ANY(COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[])))) WITH CHECK (EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = probe_v1.t AND parent.owner_id = ANY(COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))))".to_owned(),
        "CREATE POLICY proxima_platform ON publisher_health.probe_v1 FOR ALL TO CURRENT_USER USING (current_setting('app.proxima_scope', true) = 'platform') WITH CHECK (current_setting('app.proxima_scope', true) = 'platform')".to_owned(),
        "INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
             ('publisher_health.probe_v1', 'publisher-health')"
            .to_owned(),
    ];
    statements.extend(
        sidecars
            .declaration_trigger_artifacts("publisher-health")
            .expect("publisher fixture declaration triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );
    statements.extend(
        sidecars
            .presence_trigger_artifacts("publisher-health")
            .expect("publisher fixture presence triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );

    Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            PROBE_MIGRATION_VERSION,
            Cow::Borrowed("publisher health probe sidecar"),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(statements.join(";\n")).into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    }
}

#[derive(Debug)]
struct PublisherApp;

impl FlavorBundle for PublisherApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        registry.try_add_flavor(FlavorDescriptor {
            flavor_id: "publisher-health".to_owned(),
            display_name: "Publisher Health".to_owned(),
            package_version: "0".to_owned(),
            author: None,
            provenance: FlavorProvenance::Builtin,
        })?;
        registry.try_add_fact_schema::<HealthProbeV1>()?;
        registry.try_add_contract(&HEALTH_CONTRACT)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        registry.add_fact::<HealthProbeV1>();
    }

    fn migrators() -> Vec<proxima::NamedMigrator> {
        vec![proxima::NamedMigrator::new(
            "publisher-health",
            probe_migrator(),
        )]
    }
}

impl FlavorApp for PublisherApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "publisher-health-test",
            title: "Publisher Health Test",
            version: "0",
        }
    }

    fn configure(builder: RuntimeBuilder) -> RuntimeBuilder {
        builder
    }
}

async fn capture(engine: &Arc<Engine>, authz: &AuthzContext, owner: Owner, note: &str) -> Uuid {
    let probe = HealthProbeV1 {
        probe_id: Uuid::now_v7(),
        note: note.to_owned(),
    };
    engine
        .ingest_fact(
            &authenticated_context(authz.clone()),
            proxima::FactWrite::new(owner, HealthProbeV1::SCHEMA_ID, &probe),
        )
        .await
        .expect("genuine listenable Fact capture")
        .memory_id
        .into_inner()
}

#[tokio::test]
async fn publisher_health_sidecar_fixture_captures_against_real_pg() {
    if std::env::var_os("PROXIMA_TEST_PG_URL").is_none() {
        assert!(
            std::env::var("CI").as_deref() != Ok("true"),
            "PROXIMA_TEST_PG_URL required under CI=true"
        );
        eprintln!("skipping publisher health PG fixture: PROXIMA_TEST_PG_URL is unset");
        return;
    }

    let db_name = proxima_pg_testkit::unique_db_name("publisher_health_fixture");
    proxima_pg_testkit::create_db(&db_name)
        .await
        .expect("PG fixture database");
    let (runtime_url, platform_url) = proxima_pg_testkit::split_role_urls(&db_name)
        .await
        .expect("split fixture roles");
    let platform_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&platform_url)
        .await
        .expect("platform pool");
    sqlx::query("SELECT set_config('app.proxima_scope', 'platform', false)")
        .execute(&platform_pool)
        .await
        .expect("platform scope");
    let mut built = None;
    let outcome = std::panic::AssertUnwindSafe(async {
        let owner = company_owner(Uuid::now_v7());
        built = Some(
            Proxima::<PublisherApp>::app()
                .database_url(runtime_url)
                .platform_database_url(platform_url)
                .owner(owner)
                .tool_scope(ToolScope::All)
                .allow_insecure_single_owner()
                .publication(PublicationConfig::new(
                    PublicationSource::new("urn:proxima:publisher-health-tests")
                        .expect("absolute source URI"),
                ))
                .build()
                .await
                .expect("publisher health fixture builds and migrates its sidecar"),
        );
        let runtime = built.as_ref().expect("fixture was built");
        let authz = runtime.single_owner_authz().expect("single owner authz");
        let id = capture(&runtime.engine(), &authz, owner, "fixture-capture").await;
        let state: String = sqlx::query_scalar(
            "SELECT state::text FROM proxima_core.publication_outbox WHERE t = $1",
        )
        .bind(id)
        .fetch_one(&platform_pool)
        .await
        .expect("captured outbox row");
        assert_eq!(state, "pending");
    })
    .catch_unwind()
    .await;
    if let Some(built) = built {
        built.shutdown();
    }
    let _ = proxima_pg_testkit::drop_db(&db_name).await;
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}
