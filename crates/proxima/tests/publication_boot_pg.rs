//! The facade's half of the publication contract (issue #305, docs/18).
//!
//! Three properties, all of them about the SAME configuration reaching the
//! place that enforces it:
//!
//! - A bundle that freezes a listenable schema and a deployment that binds
//!   no source is refused AT BOOT, naming the schema. The check is on the
//!   unconditional engine-construction path, so no host can opt out of it.
//! - The `PROXIMA_PUBLICATION_SOURCE` / `PROXIMA_OUTBOX_*` block is read
//!   and validated by the facade, independent of any cargo feature.
//! - `PublicationConfig.limits` is the value capture enforces. There is one
//!   copy: the limits travel with the draft on the authorization witness.

use std::borrow::Cow;

use proxima::flavor::{
    FactPayload, FlavorBundle, FlavorDescriptor, FlavorProvenance, FlavorRegistry,
    FlavorRegistryError, NamedMigrator,
};
use proxima::{AppInfo, FlavorApp, Proxima, ToolScope, company_owner};
use proxima_core::flavor::{
    EmbeddingRecipe, FlavorContract, ProjectionDecl, Provenance, SchemaContract, SchemaRef,
    SearchProjectionDecl, TransferRule,
};
use proxima_core::publication::{PublicationConfig, PublicationLimits, PublicationSource};
use proxima_core::verbs::schema::PayloadKind;
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use uuid::Uuid;

/// A Fact payload that declares itself listenable — the only thing that
/// makes a publication source required.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PubProbeV1 {
    probe_id: Uuid,
    note: String,
}

impl FactPayload for PubProbeV1 {
    const SCHEMA_ID: &'static str = "pubtest/probe-v1";
    const SCHEMA_VERSION: u32 = 1;
    const LISTENABLE: bool = true;

    fn receipt_key(&self) -> Vec<u8> {
        self.probe_id.as_bytes().to_vec()
    }

    fn render(&self) -> String {
        self.note.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("pubtest.probe_v1")
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
    payload: PubProbeV1,
    row: PubProbeRow,
    kinds: [Fact],
    table: "pubtest.probe_v1",
    key: t,
    fields: {
        probe_id => probe_id: (uuid),
        note => note: (text),
    },
}

static PUB_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "pubtest",
    ordinal: 9,
    schemas: &[SchemaContract {
        id: SchemaRef::new("pubtest", "probe", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("pubtest.probe_v1"),
        search: SearchProjectionDecl::None {
            why: "a publication fixture, not a search surface",
        },
        embedding: EmbeddingRecipe::Never {
            why: "a publication fixture, not a memory",
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
        why: "a publication fixture registers no search surface",
    },
};

/// The fixture flavor's own baseline, as a real flavor ships one: the
/// sidecar table, its `flavor_surface` declaration, and the declaration and
/// presence triggers read off the frozen registrations rather than written
/// out by hand.
fn probe_migrator() -> Migrator {
    let mut registry = FlavorRegistry::new();
    ListenableApp::register(&mut registry).expect("the publication fixture registers");
    let registry = registry.try_freeze().expect("and freezes");
    let mut sidecars = proxima::flavor::PgSidecarRegistry::new();
    proxima_storage_pg::register_core_pg_sidecars(&mut sidecars);
    ListenableApp::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars
        .freeze_against(&registry)
        .expect("the fixture's PG registrations match its contract");

    let mut statements = vec![
        "CREATE SCHEMA pubtest".to_owned(),
        "CREATE TABLE pubtest.probe_v1 (
            t uuid PRIMARY KEY,
            probe_id uuid NOT NULL,
            note text NOT NULL
        )"
        .to_owned(),
        "INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
             ('pubtest.probe_v1', 'pubtest')"
            .to_owned(),
    ];
    statements.extend(
        sidecars
            .declaration_trigger_artifacts("pubtest")
            .expect("the fixture's declaration triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );
    statements.extend(
        sidecars
            .presence_trigger_artifacts("pubtest")
            .expect("the fixture's presence triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );

    Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            PROBE_MIGRATION_VERSION,
            Cow::Borrowed("publication probe sidecar"),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(statements.join(";\n")).into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    }
}

/// Host/example lane (`docs/09` §Migrations: timestamp versions ending
/// `00..=19`), so this fixture cannot collide with a first-party flavor.
const PROBE_MIGRATION_VERSION: i64 = 20_260_824_000_011;

struct ListenableApp;

impl FlavorBundle for ListenableApp {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        registry.try_add_flavor(FlavorDescriptor {
            flavor_id: "pubtest".to_string(),
            display_name: "Publication test".to_string(),
            package_version: "0".to_string(),
            author: None,
            provenance: FlavorProvenance::Builtin,
        })?;
        registry.try_add_fact_schema::<PubProbeV1>()?;
        registry.try_add_contract(&PUB_CONTRACT)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        registry.add_fact::<PubProbeV1>();
    }

    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new("pubtest", probe_migrator())]
    }
}

impl FlavorApp for ListenableApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "publication-test",
            title: "publication-test",
            version: "0",
        }
    }
}

fn source() -> PublicationSource {
    PublicationSource::new("urn:proxima:facade-test").expect("a URN is a valid source")
}

fn probe(note: &str) -> PubProbeV1 {
    PubProbeV1 {
        probe_id: Uuid::now_v7(),
        note: note.to_owned(),
    }
}

fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |key| {
        pairs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| (*v).to_string())
    }
}

#[tokio::test]
async fn a_listenable_schema_without_a_bound_source_refuses_the_boot() {
    let db_name = unique_db_name("proxima_pub_boot");
    create_db(&db_name).await.expect("PG required");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let refused = Proxima::<ListenableApp>::app()
            .database_url(db_url.clone())
            .owner(company_owner(Uuid::now_v7()))
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await
            .err()
            .ok_or("a listenable schema with no source must refuse the boot")?;
        let message = refused.to_string();
        assert!(
            message.contains(PubProbeV1::SCHEMA_ID),
            "the boot error must name the schema that made a source required, got {message}"
        );

        // The same deployment, with a source, boots.
        let built = Proxima::<ListenableApp>::app()
            .database_url(db_url)
            .owner(company_owner(Uuid::now_v7()))
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .publication(PublicationConfig::new(source()))
            .build()
            .await?;
        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("publication boot guarantee");
}

#[tokio::test]
async fn the_publication_env_block_is_read_and_validated_by_the_facade() {
    let db_name = unique_db_name("proxima_pub_env");
    create_db(&db_name).await.expect("PG required");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        // A malformed bound never reaches storage: `from_lookup` resolves
        // the whole block before a connection is opened.
        let refused = Proxima::<ListenableApp>::app()
            .from_lookup(env(&[
                ("PROXIMA_PUBLICATION_SOURCE", "urn:proxima:facade-test"),
                ("PROXIMA_OUTBOX_MAX_PENDING", "not-a-number"),
            ]))
            .err()
            .ok_or("a malformed outbox bound must refuse the boot")?;
        assert!(
            refused.to_string().contains("PROXIMA_OUTBOX_MAX_PENDING"),
            "the error must name the offending key, got {refused}"
        );

        let refused = Proxima::<ListenableApp>::app()
            .from_lookup(env(&[("PROXIMA_PUBLICATION_SOURCE", "relative/reference")]))
            .err()
            .ok_or("a relative source must refuse the boot")?;
        assert!(
            refused.to_string().contains("PROXIMA_PUBLICATION_SOURCE"),
            "the error must name the offending key, got {refused}"
        );

        // The well-formed block satisfies the listenable schema's demand
        // for a source through the environment alone.
        let built = Proxima::<ListenableApp>::app()
            .from_lookup(env(&[
                ("PROXIMA_PUBLICATION_SOURCE", "urn:proxima:facade-test"),
                ("PROXIMA_OUTBOX_MAX_PENDING", "12"),
                ("PROXIMA_OUTBOX_MAX_PAYLOAD_BYTES", "4096"),
            ]))?
            .database_url(db_url)
            .owner(company_owner(Uuid::now_v7()))
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .build()
            .await?;
        let limits = built.engine().publication_config().limits;
        assert_eq!(limits.max_pending, 12);
        assert_eq!(limits.max_payload_bytes, 4096);
        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("publication env block");
}

#[tokio::test]
async fn the_configured_payload_ceiling_is_the_one_capture_enforces() {
    let db_name = unique_db_name("proxima_pub_limit");
    create_db(&db_name).await.expect("PG required");
    let db_url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxima::<ListenableApp>::app()
            .database_url(db_url)
            .owner(owner)
            .allow_insecure_single_owner()
            .tool_scope(ToolScope::All)
            .publication(
                PublicationConfig::new(source()).with_limits(PublicationLimits {
                    max_pending: proxima_core::publication::DEFAULT_MAX_PENDING,
                    max_payload_bytes: 4096,
                }),
            )
            .build()
            .await?;
        let authz = built.single_owner_authz().ok_or("single owner")?;
        let engine = built.engine();

        // Under the ceiling: admitted, and its event is captured.
        engine
            .ingest_fact(
                &authz,
                proxima::FactWrite::new(owner, PubProbeV1::SCHEMA_ID, &probe("small")),
            )
            .await?;
        let captured: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.publication_outbox")
                .fetch_one(built.pool_for_tests())
                .await?;
        assert_eq!(captured, 1, "the small export is captured");

        // Five KiB of note seals past the 4 KiB ceiling the FACADE set.
        // Before the limits travelled with the draft this write succeeded:
        // storage held its own default-valued copy and never saw this one.
        let oversized = engine
            .ingest_fact(
                &authz,
                proxima::FactWrite::new(
                    owner,
                    PubProbeV1::SCHEMA_ID,
                    &probe(&"x".repeat(5 * 1024)),
                ),
            )
            .await
            .err()
            .ok_or("a 5 KiB export must be refused under a 4 KiB ceiling")?;
        let message = oversized.to_string();
        assert!(
            message.contains("over the 4096-byte limit"),
            "the refusal must name the ceiling the FACADE configured, got {message}"
        );

        let after: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.publication_outbox")
                .fetch_one(built.pool_for_tests())
                .await?;
        assert_eq!(after, 1, "the refused write captured nothing");
        let memories: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory")
            .fetch_one(built.pool_for_tests())
            .await?;
        assert_eq!(memories, 1, "and admitted no Fact");

        built.shutdown();
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("configured capture ceiling");
}
