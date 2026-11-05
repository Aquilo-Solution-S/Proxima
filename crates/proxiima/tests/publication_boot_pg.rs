//! The facade's half of the publication contract (issue #305, docs/host-integration.md).
//!
//! Three properties, all of them about the SAME configuration reaching the
//! place that enforces it:
//!
//! - A bundle that freezes a listenable schema and a deployment that binds
//!   no source is refused AT BOOT, naming the schema. The check is on the
//!   unconditional engine-construction path, so no host can opt out of it.
//! - The `PROXIIMA_PUBLICATION_SOURCE` / `PROXIIMA_OUTBOX_*` block is read
//!   and validated by the facade, independent of any cargo feature.
//! - `PublicationConfig.limits` is the value capture enforces. There is one
//!   copy: the limits travel with the draft on the authorization witness.

#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

use std::borrow::Cow;

use proxiima::spiice::{
    FactPayload, NamedMigrator, SpiiceBundle, SpiiceDescriptor, SpiiceProvenance, SpiiceRegistry,
    SpiiceRegistryError,
};
use proxiima::{AppInfo, Proxiima, ServedSurface, SpiiceApp, company_owner};
use proxiima_core::env_from_pairs as env;
use proxiima_core::publication::{PublicationConfig, PublicationLimits, PublicationSource};
use proxiima_core::spiice::{
    EmbeddingRecipe, ProjectionDecl, Provenance, SchemaContract, SchemaRef, SearchProjectionDecl,
    SpiiceContract, TransferRule,
};
use proxiima_core::verbs::schema::PayloadKind;
use split_core_db::clone_split_core_db;
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

impl proxiima::spiice::PayloadProjection for PubProbeV1 {
    const KEY_FIELDS: &'static [proxiima::spiice::KeyField<Self>] = &[];
    fn display(&self) -> proxiima::spiice::PayloadDisplay {
        proxiima::spiice::PayloadDisplay {
            body: Some(self.note.clone()),
            ..Default::default()
        }
    }
    fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
        let mut builder = proxiima::spiice::DurablePayloadBuilder::new();
        builder
            .field("probe_id", &self.probe_id)?
            .field("note", &self.note)?;
        Ok(builder.finish())
    }
}

proxiima::spiice::pg_sidecar! {
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

static PUB_CONTRACT: SpiiceContract = SpiiceContract {
    retired_facts: &[],
    spiice_id: "pubtest",
    ordinal: 9,
    schemas: &[SchemaContract {
        id: SchemaRef::new("pubtest", "probe", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("pubtest.probe_v1"),
        search: SearchProjectionDecl::None {
            why: "a publication fixture, not a search surface",
            tag_column: None,
        },
        embedding: EmbeddingRecipe::Never {
            why: "a publication fixture, not a memory",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
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

/// The fixture spiice's own baseline, as a real spiice ships one: the
/// sidecar table, its `spiice_surface` declaration, and the declaration and
/// presence triggers read off the frozen registrations rather than written
/// out by hand.
fn probe_migrator() -> Migrator {
    let mut registry = SpiiceRegistry::new();
    ListenableApp::register(&mut registry).expect("the publication fixture registers");
    let registry = registry.try_freeze().expect("and freezes");
    let mut sidecars = proxiima::spiice::PgSidecarRegistry::new();
    proxiima_storage_pg::register_core_pg_sidecars(&mut sidecars);
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
        "INSERT INTO proxiima_core.spiice_surface (table_name, spiice_id) VALUES
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
    statements.push(
        "ALTER TABLE pubtest.probe_v1 ENABLE ROW LEVEL SECURITY;\n\
         ALTER TABLE pubtest.probe_v1 FORCE ROW LEVEL SECURITY;\n\
         CREATE POLICY proxiima_owner_read ON pubtest.probe_v1 FOR SELECT TO PUBLIC USING (EXISTS (SELECT 1 FROM proxiima_core.memory m WHERE m.t = t AND m.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])));\n\
         CREATE POLICY proxiima_owner_write ON pubtest.probe_v1 FOR ALL TO PUBLIC USING (EXISTS (SELECT 1 FROM proxiima_core.memory m WHERE m.t = t AND m.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[]))) WITH CHECK (EXISTS (SELECT 1 FROM proxiima_core.memory m WHERE m.t = t AND m.owner_id = ANY((SELECT COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))::uuid[])));\n\
         CREATE POLICY proxiima_platform ON pubtest.probe_v1 FOR ALL TO CURRENT_USER USING (current_setting('app.proxiima_scope', true) = 'platform') WITH CHECK (current_setting('app.proxiima_scope', true) = 'platform')"
            .to_owned(),
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

/// Host/example lane (`docs/build-a-spiice.md` §Migrations: timestamp versions ending
/// `00..=19`), so this fixture cannot collide with a first-party spiice.
const PROBE_MIGRATION_VERSION: i64 = 20_260_824_000_011;

struct ListenableApp;

impl SpiiceBundle for ListenableApp {
    fn register(registry: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        registry.try_add_spiice(SpiiceDescriptor {
            spiice_id: "pubtest".to_string(),
            display_name: "Publication test".to_string(),
            package_version: "0".to_string(),
            author: None,
            provenance: SpiiceProvenance::Builtin,
        })?;
        registry.try_add_fact_schema::<PubProbeV1>()?;
        registry.try_add_contract(&PUB_CONTRACT)
    }

    fn register_pg_sidecars(registry: &mut proxiima::spiice::PgSidecarRegistry) {
        registry.add_fact::<PubProbeV1>();
    }

    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new("pubtest", probe_migrator())]
    }
}

impl SpiiceApp for ListenableApp {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "publication-test",
            title: "publication-test",
            version: "0",
        }
    }
}

fn source() -> PublicationSource {
    PublicationSource::new("urn:proxiima:facade-test").expect("a URN is a valid source")
}

fn probe(note: &str) -> PubProbeV1 {
    PubProbeV1 {
        probe_id: Uuid::now_v7(),
        note: note.to_owned(),
    }
}

async fn admin_pool(database: &str) -> Result<sqlx::PgPool, sqlx::Error> {
    sqlx::PgPool::connect(&proxiima_pg_testkit::db_url(database)).await
}

#[tokio::test]
async fn a_listenable_schema_without_a_bound_source_refuses_the_boot() {
    let split_db = clone_split_core_db("proxiima_pub_boot")
        .await
        .expect("PG required");
    let (db_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let refused = Proxiima::<ListenableApp>::app()
            .database_url(db_url.clone())
            .store_number(0)
            .platform_database_url(platform_url.clone())
            .owner(company_owner(Uuid::now_v7()))
            .allow_insecure_single_owner()
            .served_surface(ServedSurface::All)
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
        let built = Proxiima::<ListenableApp>::app()
            .database_url(db_url)
            .store_number(0)
            .platform_database_url(platform_url)
            .owner(company_owner(Uuid::now_v7()))
            .allow_insecure_single_owner()
            .served_surface(ServedSurface::All)
            .publication(PublicationConfig::new(source()))
            .build()
            .await?;
        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("publication boot guarantee");
}

#[tokio::test]
async fn the_publication_env_block_is_read_and_validated_by_the_facade() {
    let split_db = clone_split_core_db("proxiima_pub_env")
        .await
        .expect("PG required");
    let (db_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        // A malformed bound never reaches storage: `from_lookup` resolves
        // the whole block before a connection is opened.
        let refused = Proxiima::<ListenableApp>::app()
            .from_lookup(env(&[
                ("PROXIIMA_PUBLICATION_SOURCE", "urn:proxiima:facade-test"),
                ("PROXIIMA_OUTBOX_MAX_PENDING", "not-a-number"),
            ]))
            .err()
            .ok_or("a malformed outbox bound must refuse the boot")?;
        assert!(
            refused.to_string().contains("PROXIIMA_OUTBOX_MAX_PENDING"),
            "the error must name the offending key, got {refused}"
        );

        let refused = Proxiima::<ListenableApp>::app()
            .from_lookup(env(&[(
                "PROXIIMA_PUBLICATION_SOURCE",
                "relative/reference",
            )]))
            .err()
            .ok_or("a relative source must refuse the boot")?;
        assert!(
            refused.to_string().contains("PROXIIMA_PUBLICATION_SOURCE"),
            "the error must name the offending key, got {refused}"
        );

        // The well-formed block satisfies the listenable schema's demand
        // for a source through the environment alone.
        let built = Proxiima::<ListenableApp>::app()
            .from_lookup(env(&[
                ("PROXIIMA_PUBLICATION_SOURCE", "urn:proxiima:facade-test"),
                ("PROXIIMA_OUTBOX_MAX_PENDING", "12"),
                ("PROXIIMA_OUTBOX_MAX_PAYLOAD_BYTES", "4096"),
            ]))?
            .database_url(db_url)
            .store_number(0)
            .platform_database_url(platform_url)
            .owner(company_owner(Uuid::now_v7()))
            .allow_insecure_single_owner()
            .served_surface(ServedSurface::All)
            .build()
            .await?;
        let limits = built.host().engine().publication_config().limits;
        assert_eq!(limits.max_pending, 12);
        assert_eq!(limits.max_payload_bytes, 4096);
        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("publication env block");
}

/// Capture is downstream of authorization, so a refused write leaves
/// nothing to publish.
///
/// The intake's only input is `proxiima_core.publication_outbox`: the drain
/// claims from it and hands over the `envelope` column verbatim
/// (`in_process_outbox_tests.rs` proves that half). An empty table after a
/// denial is therefore the whole claim — there is no second path from a Fact
/// write to the intake.
#[tokio::test]
async fn an_unauthorized_listenable_write_captures_nothing() {
    let split_db = clone_split_core_db("proxiima_pub_authz")
        .await
        .expect("PG required");
    let db_name = split_db.name().to_owned();
    let (db_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxiima::<ListenableApp>::app()
            .database_url(db_url)
            .store_number(0)
            .platform_database_url(platform_url)
            .owner(owner)
            .allow_insecure_single_owner()
            .served_surface(ServedSurface::All)
            .publication(PublicationConfig::new(source()))
            .build()
            .await?;
        let authz = proxiima_core::test_fixtures::authenticated_context(
            built.single_owner_authz().ok_or("single owner")?,
        );
        let engine = built.host().engine();

        // An owner this caller holds no grant on. The listenable schema,
        // the source and the ceiling are all exactly the ones the
        // authorized write below succeeds under, so the ONLY difference is
        // the authorization.
        let stranger = company_owner(Uuid::now_v7());
        let refused = engine
            .ingest_fact(
                &authz,
                proxiima::FactWrite::new(stranger, PubProbeV1::SCHEMA_ID, &probe("not mine")),
            )
            .await
            .err()
            .ok_or("a write for an owner the caller has no grant on must be refused")?;
        assert_eq!(
            refused.code,
            proxiima_core::ErrorCode::Forbidden,
            "the refusal must be an authorization one, got {refused}"
        );

        let captured: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxiima_core.publication_outbox")
                .fetch_one(&admin_pool(&db_name).await?)
                .await?;
        assert_eq!(captured, 0, "a denied write must capture no event");
        let memories: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxiima_core.memory")
            .fetch_one(&admin_pool(&db_name).await?)
            .await?;
        assert_eq!(memories, 0, "and admit no Fact");

        // The control: the same write, for the owner the caller DOES hold,
        // captures. Without it the assertions above would also hold for a
        // fixture that captures nothing at all.
        engine
            .ingest_fact(
                &authz,
                proxiima::FactWrite::new(owner, PubProbeV1::SCHEMA_ID, &probe("mine")),
            )
            .await?;
        let captured: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxiima_core.publication_outbox")
                .fetch_one(&admin_pool(&db_name).await?)
                .await?;
        assert_eq!(captured, 1, "the authorized write is captured");
        let sidecars: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM pubtest.probe_v1")
            .fetch_one(&admin_pool(&db_name).await?)
            .await?;
        assert_eq!(
            sidecars, 1,
            "the standalone Fact keeps its registered sidecar"
        );

        let mut uow = engine.unit_of_work(&authz).await?;
        uow.ingest_fact(proxiima::FactWrite::new(
            owner,
            PubProbeV1::SCHEMA_ID,
            &probe("uow"),
        ))
        .await?;
        uow.commit().await?;
        let captured: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxiima_core.publication_outbox")
                .fetch_one(&admin_pool(&db_name).await?)
                .await?;
        assert_eq!(captured, 2, "the composed UoW Fact is captured");
        let sidecars: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM pubtest.probe_v1")
            .fetch_one(&admin_pool(&db_name).await?)
            .await?;
        assert_eq!(sidecars, 2, "the composed UoW Fact keeps its sidecar");

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("authorized capture only");
}

#[tokio::test]
async fn the_configured_payload_ceiling_is_the_one_capture_enforces() {
    let split_db = clone_split_core_db("proxiima_pub_limit")
        .await
        .expect("PG required");
    let db_name = split_db.name().to_owned();
    let (db_url, platform_url) = (
        split_db.runtime_url().to_owned(),
        split_db.platform_url().to_owned(),
    );

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = company_owner(Uuid::now_v7());
        let built = Proxiima::<ListenableApp>::app()
            .database_url(db_url)
            .store_number(0)
            .platform_database_url(platform_url)
            .owner(owner)
            .allow_insecure_single_owner()
            .served_surface(ServedSurface::All)
            .publication(
                PublicationConfig::new(source()).with_limits(PublicationLimits {
                    max_pending: proxiima_core::publication::DEFAULT_MAX_PENDING,
                    max_payload_bytes: 4096,
                }),
            )
            .build()
            .await?;
        let authz = proxiima_core::test_fixtures::authenticated_context(
            built.single_owner_authz().ok_or("single owner")?,
        );
        let engine = built.host().engine();

        // Under the ceiling: admitted, and its event is captured.
        engine
            .ingest_fact(
                &authz,
                proxiima::FactWrite::new(owner, PubProbeV1::SCHEMA_ID, &probe("small")),
            )
            .await?;
        let captured: i64 =
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxiima_core.publication_outbox")
                .fetch_one(&admin_pool(&db_name).await?)
                .await?;
        assert_eq!(captured, 1, "the small export is captured");

        // Five KiB of note seals past the 4 KiB ceiling the FACADE set.
        // Before the limits travelled with the draft this write succeeded:
        // storage held its own default-valued copy and never saw this one.
        let oversized = engine
            .ingest_fact(
                &authz,
                proxiima::FactWrite::new(
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
            sqlx::query_scalar("SELECT count(*)::bigint FROM proxiima_core.publication_outbox")
                .fetch_one(&admin_pool(&db_name).await?)
                .await?;
        assert_eq!(after, 1, "the refused write captured nothing");
        let memories: i64 = sqlx::query_scalar("SELECT count(*)::bigint FROM proxiima_core.memory")
            .fetch_one(&admin_pool(&db_name).await?)
            .await?;
        assert_eq!(memories, 1, "and admitted no Fact");

        built.shutdown().await;
        Ok(())
    }
    .await;

    result.expect("configured capture ceiling");
}
