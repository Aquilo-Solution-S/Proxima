//! The probe flavor the outbox runtime tests capture Facts through: one
//! listenable schema, its sidecar table, and the app that boots them. Shared by
//! the in-process delivery tests, which capture through the real Postgres
//! path.

use std::borrow::Cow;
use std::time::Duration;

use proxima_core::flavor::{
    EmbeddingRecipe, FlavorContract, ProjectionDecl, Provenance, SchemaContract, SchemaRef,
    SearchProjectionDecl, TransferRule,
};
use proxima_core::test_fixtures::authenticated_context;
use proxima_core::{FactPayload, OwnerId};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use uuid::Uuid;

pub(crate) const PROBE_SOURCE: &str = "urn:proxima:runtime-publisher-supervision";
const PROBE_MIGRATION_VERSION: i64 = 20_260_920_000_011;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct OutboxProbeV1 {
    probe_id: Uuid,
    note: String,
}

impl FactPayload for OutboxProbeV1 {
    const SCHEMA_ID: &'static str = "publisher-supervision/probe-v1";
    const SCHEMA_VERSION: u32 = 1;
    const LISTENABLE: bool = true;

    fn receipt_key(&self) -> Vec<u8> {
        self.probe_id.as_bytes().to_vec()
    }

    fn render(&self) -> String {
        self.note.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("publisher_supervision.probe_v1")
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

crate::flavor::pg_sidecar! {
    payload: OutboxProbeV1,
    row: OutboxProbeRow,
    kinds: [Fact],
    table: "publisher_supervision.probe_v1",
    key: t,
    fields: {
        probe_id => probe_id: (uuid),
        note => note: (text),
    },
}

static HEALTH_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "publisher-supervision",
    ordinal: 19,
    schemas: &[SchemaContract {
        id: SchemaRef::new("publisher-supervision", "probe", 1),
        kind: proxima_core::verbs::schema::PayloadKind::Fact,
        sidecar_table: Some("publisher_supervision.probe_v1"),
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
    use crate::flavor::FlavorBundle;

    let mut registry = crate::flavor::FlavorRegistry::new();
    OutboxProbeApp::register(&mut registry).expect("publisher fixture registers");
    let registry = registry.try_freeze().expect("publisher fixture freezes");
    let mut sidecars = crate::flavor::PgSidecarRegistry::new();
    proxima_storage_pg::register_core_pg_sidecars(&mut sidecars);
    OutboxProbeApp::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars
        .freeze_against(&registry)
        .expect("publisher fixture PG sidecar matches its contract");

    let mut statements = vec![
        "CREATE SCHEMA publisher_supervision".to_owned(),
        "CREATE TABLE publisher_supervision.probe_v1 (
            t uuid PRIMARY KEY,
            probe_id uuid NOT NULL,
            note text NOT NULL
        )"
        .to_owned(),
        "ALTER TABLE publisher_supervision.probe_v1 ENABLE ROW LEVEL SECURITY".to_owned(),
        "ALTER TABLE publisher_supervision.probe_v1 FORCE ROW LEVEL SECURITY".to_owned(),
        "CREATE POLICY proxima_owner_read ON publisher_supervision.probe_v1 FOR SELECT TO PUBLIC USING (EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = probe_v1.t AND parent.owner_id = ANY(COALESCE(NULLIF(current_setting('app.owner', true), '')::uuid[], '{}'::uuid[]))))".to_owned(),
        "CREATE POLICY proxima_owner_write ON publisher_supervision.probe_v1 FOR ALL TO PUBLIC USING (EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = probe_v1.t AND parent.owner_id = ANY(COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[])))) WITH CHECK (EXISTS (SELECT 1 FROM proxima_core.memory AS parent WHERE parent.t = probe_v1.t AND parent.owner_id = ANY(COALESCE(NULLIF(current_setting('app.write_owner', true), '')::uuid[], '{}'::uuid[]))))".to_owned(),
        "CREATE POLICY proxima_platform ON publisher_supervision.probe_v1 FOR ALL TO CURRENT_USER USING (current_setting('app.proxima_scope', true) = 'platform') WITH CHECK (current_setting('app.proxima_scope', true) = 'platform')".to_owned(),
        "INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
             ('publisher_supervision.probe_v1', 'publisher-supervision')"
            .to_owned(),
    ];
    statements.extend(
        sidecars
            .declaration_trigger_artifacts("publisher-supervision")
            .expect("publisher fixture declaration triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );
    statements.extend(
        sidecars
            .presence_trigger_artifacts("publisher-supervision")
            .expect("publisher fixture presence triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );

    Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            PROBE_MIGRATION_VERSION,
            Cow::Borrowed("publisher supervision probe sidecar"),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(statements.join(";\n")).into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    }
}

#[derive(Debug)]
pub(crate) struct OutboxProbeApp;

impl crate::flavor::FlavorBundle for OutboxProbeApp {
    fn register(
        registry: &mut crate::flavor::FlavorRegistry,
    ) -> Result<(), crate::flavor::FlavorRegistryError> {
        registry.try_add_flavor(crate::flavor::FlavorDescriptor {
            flavor_id: "publisher-supervision".to_owned(),
            display_name: "Publisher Supervision".to_owned(),
            package_version: "0".to_owned(),
            author: None,
            provenance: crate::flavor::FlavorProvenance::Builtin,
        })?;
        registry.try_add_fact_schema::<OutboxProbeV1>()?;
        registry.try_add_contract(&HEALTH_CONTRACT)
    }

    fn register_pg_sidecars(registry: &mut crate::flavor::PgSidecarRegistry) {
        registry.add_fact::<OutboxProbeV1>();
    }

    fn migrators() -> Vec<crate::NamedMigrator> {
        vec![crate::NamedMigrator::new(
            "publisher-supervision",
            probe_migrator(),
        )]
    }
}

impl crate::FlavorApp for OutboxProbeApp {
    fn app_info() -> crate::AppInfo {
        crate::AppInfo {
            id: "publisher-supervision-test",
            title: "Publisher Supervision Test",
            version: "0",
        }
    }

    fn configure(builder: crate::RuntimeBuilder) -> crate::RuntimeBuilder {
        builder
    }
}

pub(crate) async fn wait_for_outbox(pool: &sqlx::PgPool, t: Uuid, wanted: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let state: String = sqlx::query_scalar(
                "SELECT state::text FROM proxima_core.publication_outbox WHERE t = $1",
            )
            .bind(t)
            .fetch_one(pool)
            .await
            .expect("outbox state query");
            if state == wanted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("outbox state changes");
}

pub(crate) async fn capture(built: &crate::BuiltProxima, owner: OwnerId, note: &str) -> Uuid {
    let payload = OutboxProbeV1 {
        probe_id: Uuid::now_v7(),
        note: note.to_owned(),
    };
    built
        .host()
        .engine()
        .ingest_fact(
            &authenticated_context(built.single_owner_authz().expect("single owner authz")),
            crate::FactWrite::new(owner, OutboxProbeV1::SCHEMA_ID, &payload),
        )
        .await
        .expect("genuine publication Fact")
        .memory_id
        .into_inner()
}
