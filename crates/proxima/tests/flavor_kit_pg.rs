//! The flavor plumbing a flavor crate no longer hand-writes, end to end:
//!
//! - `flavor_bundle!` composes a flavor with no hand-written impl;
//! - its migration's owner RLS is one `proxima_core.install_owner_rls` call,
//!   and the split-role boot's runtime RLS guard accepts the result;
//! - `NamedMigrator::flavor` gives it its own ledger, and a flavor that ran on
//!   core's shared ledger moves onto its own without re-running anything;
//! - the test support comes from `proxima::testkit` alone: split-role
//!   databases, an owner-scoped authz, and the trigger drift assertion.

use std::borrow::Cow;

use proxima::flavor::{
    FactPayload, FlavorBundle, FlavorRegistry, FlavorRegistryError, NamedMigrator,
    PgSidecarRegistry,
};
use proxima::testkit::{SplitRoleDb, assert_trigger_migrations, scoped_authz};
use proxima::{AppInfo, FlavorApp, Proxima, QueryRequest, ToolScope, company_owner};
use proxima_core::flavor::{
    EmbeddingRecipe, FlavorContract, ProjectionDecl, Provenance, SchemaContract, SchemaRef,
    SearchProjectionDecl, TransferRule,
};
use proxima_core::verbs::schema::PayloadKind;
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct KitNoteV1 {
    note_id: Uuid,
    body: String,
}

impl FactPayload for KitNoteV1 {
    const SCHEMA_ID: &'static str = "kittest/note-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        self.note_id.as_bytes().to_vec()
    }

    fn render(&self) -> String {
        self.body.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("kittest.note_v1")
    }
}

proxima::flavor::pg_sidecar! {
    payload: KitNoteV1,
    row: KitNoteRow,
    kinds: [Fact],
    table: "kittest.note_v1",
    key: t,
    fields: {
        note_id => note_id: (uuid),
        body => body: (text),
    },
}

static KIT_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "kittest",
    ordinal: 11,
    schemas: &[SchemaContract {
        id: SchemaRef::new("kittest", "note", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("kittest.note_v1"),
        search: SearchProjectionDecl::None {
            why: "a plumbing fixture, not a search surface",
        },
        embedding: EmbeddingRecipe::Never {
            why: "a plumbing fixture, not a memory",
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
        why: "a plumbing fixture registers no search surface",
    },
};

/// Host/example lane (`docs/09` §Migrations: timestamp versions ending
/// `00..=19`).
const KIT_MIGRATION_VERSION: i64 = 20_260_924_000_012;

/// The fixture's baseline as a flavor ships one. Its whole owner-RLS
/// section is the installer call; `leave_settings_unclassified` drops one
/// table from the classification.
fn kit_sql(leave_settings_unclassified: bool) -> String {
    let mut registry = FlavorRegistry::new();
    kit::register(&mut registry).expect("the kit fixture registers");
    let registry = registry.try_freeze().expect("and freezes");
    let mut sidecars = PgSidecarRegistry::new();
    proxima::flavor::register_core_pg_sidecars(&mut sidecars);
    kit::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars
        .freeze_against(&registry)
        .expect("the fixture's PG registrations match its contract");

    let ownerless = if leave_settings_unclassified {
        "'{}'"
    } else {
        "ARRAY['settings']"
    };
    let mut statements = vec![
        "CREATE SCHEMA kittest".to_owned(),
        "CREATE TABLE kittest.note_v1 (
            t uuid PRIMARY KEY REFERENCES proxima_core.memory (t),
            note_id uuid NOT NULL,
            body text NOT NULL
        )"
        .to_owned(),
        "CREATE TABLE kittest.sync_cursor (owner_id uuid PRIMARY KEY, position bigint NOT NULL)"
            .to_owned(),
        "CREATE TABLE kittest.settings (key text PRIMARY KEY, value text NOT NULL)".to_owned(),
        "INSERT INTO proxima_core.flavor_surface (table_name, flavor_id) VALUES
             ('kittest.note_v1', 'kittest')"
            .to_owned(),
    ];
    statements.extend(
        sidecars
            .declaration_trigger_artifacts("kittest")
            .expect("declaration triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );
    statements.push(format!(
        "SELECT proxima_core.install_owner_rls('kittest', ARRAY['sync_cursor'], ARRAY['note_v1'], {ownerless})"
    ));
    statements.extend(
        sidecars
            .presence_trigger_artifacts("kittest")
            .expect("presence triggers")
            .into_iter()
            .map(|artifact| artifact.forward),
    );
    statements.join(";\n")
}

fn kit_migrator(leave_settings_unclassified: bool) -> Migrator {
    Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            KIT_MIGRATION_VERSION,
            Cow::Borrowed("kit baseline"),
            MigrationType::Simple,
            sqlx::AssertSqlSafe(kit_sql(leave_settings_unclassified)).into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    }
}

mod kit {
    proxima::flavor_bundle! {
        bundle = KitFlavor,
        name = "kittest",
        display_name = "Kit",
        fact_schemas = [super::KitNoteV1],
        contract = &super::KIT_CONTRACT,
        migrations = super::kit_migrator(false),
        app = { title = "Kit fixture", version = "0" },
    }
}

mod kit_gap {
    proxima::flavor_bundle! {
        bundle = KitGapFlavor,
        name = "kittest",
        fact_schemas = [super::KitNoteV1],
        contract = &super::KIT_CONTRACT,
        migrations = super::kit_migrator(true),
        app = { title = "Kit fixture, one table unclassified" },
    }
}

/// The same flavor as a pre-v0.0.18 flavor wrote it by hand, on core's
/// shared ledger.
struct SharedLedgerKit;

impl FlavorBundle for SharedLedgerKit {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        kit::register(registry)
    }

    fn register_pg_sidecars(registry: &mut PgSidecarRegistry) {
        kit::register_pg_sidecars(registry);
    }

    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new("kittest", kit_migrator(false))]
    }
}

impl FlavorApp for SharedLedgerKit {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "kittest",
            title: "Kit fixture, shared ledger",
            version: "0",
        }
    }
}

async fn ledger_versions(db: &SplitRoleDb, table: &str) -> Option<Vec<i64>> {
    let admin = sqlx::PgPool::connect(&db.admin_url())
        .await
        .expect("admin pool");
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(table)
        .fetch_one(&admin)
        .await
        .expect("ledger probe");
    let versions = if exists {
        let query = match table {
            "public._sqlx_migrations" => sqlx::query_scalar(
                "SELECT version FROM public._sqlx_migrations WHERE version > 9999 ORDER BY version",
            ),
            "public._sqlx_migrations_kittest" => sqlx::query_scalar(
                "SELECT version FROM public._sqlx_migrations_kittest ORDER BY version",
            ),
            other => panic!("no ledger query for {other}"),
        };
        Some(query.fetch_all(&admin).await.expect("ledger rows"))
    } else {
        None
    };
    admin.close().await;
    versions
}

#[test]
fn the_bundle_is_the_hand_written_one() {
    assert_eq!(
        kit::KitFlavor::app_info(),
        AppInfo {
            id: "kittest",
            title: "Kit fixture",
            version: "0",
        }
    );
    let migrators = <kit::KitFlavor as FlavorBundle>::migrators();
    assert_eq!(migrators.len(), 1);
    assert_eq!(migrators[0].source(), "kittest");
    assert_eq!(
        migrators[0].migrator().table_name,
        "public._sqlx_migrations_kittest"
    );
    assert!(migrators[0].migrator().ignore_missing);
    assert_eq!(kit_gap::KitGapFlavor::app_info().id, "kittest");

    let mut registry = PgSidecarRegistry::new();
    <kit::KitFlavor as FlavorBundle>::register_pg_sidecars(&mut registry);
    let mut hand_written = PgSidecarRegistry::new();
    hand_written.add_fact::<KitNoteV1>();
    assert_eq!(format!("{registry:?}"), format!("{hand_written:?}"));

    assert_trigger_migrations::<kit::KitFlavor>("kittest", &[&kit_sql(false)]);
}

#[test]
#[should_panic(expected = "appear in no migration")]
fn the_trigger_drift_assertion_names_a_missing_trigger() {
    let without_presence = kit_sql(false)
        .split(";\n")
        .filter(|statement| !statement.contains("assert_declared_sidecar_present"))
        .collect::<Vec<_>>()
        .join(";\n");
    assert_trigger_migrations::<kit::KitFlavor>("kittest", &[&without_presence]);
}

#[tokio::test]
async fn an_installer_migration_boots_under_the_runtime_rls_guard() {
    let db = SplitRoleDb::create("proxima_flavor_kit", &[])
        .await
        .expect("PG required");
    let owner = company_owner(Uuid::now_v7());
    let built = Proxima::<kit::KitFlavor>::app()
        .database_url(db.runtime_url())
        .platform_database_url(db.platform_url())
        .owner(owner)
        .allow_insecure_single_owner()
        .tool_scope(ToolScope::All)
        .build()
        .await
        .expect("an installer-only owner-RLS migration passes the runtime RLS guard");

    let authz = scoped_authz(owner);
    let note = KitNoteV1 {
        note_id: Uuid::now_v7(),
        body: "written under owner RLS".to_owned(),
    };
    let engine = built.engine();
    let outcome = engine
        .ingest_fact(
            &authz,
            proxima::FactWrite::new(owner, "kittest/boot", &note),
        )
        .await
        .expect("the FK-parent write policy admits the owner's sidecar row");
    let response = engine
        .query(&authz, &QueryRequest::readable())
        .await
        .expect("owner query");
    assert!(
        response
            .memories
            .iter()
            .any(|memory| memory.id == outcome.memory_id),
        "the owner reads its own note back"
    );
    built.shutdown();

    assert_eq!(
        ledger_versions(&db, "public._sqlx_migrations_kittest").await,
        Some(vec![KIT_MIGRATION_VERSION]),
        "the flavor records on its own ledger"
    );
    assert_eq!(
        ledger_versions(&db, "public._sqlx_migrations").await,
        Some(Vec::new()),
        "and not on core's"
    );
}

#[tokio::test]
async fn an_unclassified_table_refuses_the_migration() {
    let db = SplitRoleDb::create("proxima_flavor_kit_gap", &[])
        .await
        .expect("PG required");
    let refused = Proxima::<kit_gap::KitGapFlavor>::app()
        .database_url(db.runtime_url())
        .platform_database_url(db.platform_url())
        .owner(company_owner(Uuid::now_v7()))
        .allow_insecure_single_owner()
        .tool_scope(ToolScope::All)
        .build()
        .await
        .expect_err("an unclassified table must refuse the boot");
    let message = refused.to_string();
    assert!(
        message.contains("owner RLS classification missing for kittest.settings"),
        "the refusal names the table: {message}"
    );
}

#[tokio::test]
async fn a_shared_ledger_flavor_moves_to_its_own_ledger_without_rerunning() {
    let db = SplitRoleDb::create("proxima_flavor_kit_cutover", &[])
        .await
        .expect("PG required");
    let owner = company_owner(Uuid::now_v7());
    let shared = Proxima::<SharedLedgerKit>::app()
        .database_url(db.runtime_url())
        .platform_database_url(db.platform_url())
        .owner(owner)
        .allow_insecure_single_owner()
        .tool_scope(ToolScope::All)
        .build()
        .await
        .expect("a shared-ledger flavor still boots");
    shared.shutdown();
    assert_eq!(
        ledger_versions(&db, "public._sqlx_migrations").await,
        Some(vec![KIT_MIGRATION_VERSION]),
        "before: the flavor's row sits on core's ledger"
    );
    assert_eq!(
        ledger_versions(&db, "public._sqlx_migrations_kittest").await,
        None
    );

    // Re-running the baseline would fail on `CREATE SCHEMA kittest`, so a
    // clean boot proves the row moved instead of the migration re-running.
    let own = Proxima::<kit::KitFlavor>::app()
        .database_url(db.runtime_url())
        .platform_database_url(db.platform_url())
        .owner(owner)
        .allow_insecure_single_owner()
        .tool_scope(ToolScope::All)
        .build()
        .await
        .expect("the same flavor on NamedMigrator::flavor boots over the shared-ledger database");
    own.shutdown();
    assert_eq!(
        ledger_versions(&db, "public._sqlx_migrations_kittest").await,
        Some(vec![KIT_MIGRATION_VERSION]),
        "after: the row is on the flavor's own ledger"
    );
    assert_eq!(
        ledger_versions(&db, "public._sqlx_migrations").await,
        Some(Vec::new()),
        "and gone from core's"
    );
}
