//! Automatic Fact keys follow the declared SQL types and current ownership.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use std::sync::Arc;

use proxiima::spiice::{
    CounterRule, DbConstraint, EmbeddingRecipe, EraseRule, ExportRule, FactPayload, ForgetRule,
    KeyShape, PayloadKeyBuilder, PayloadKind, PgSidecarRegistry, ProjectionDecl, Provenance,
    SchemaContract, SchemaRef, SearchProjectionDecl, SidecarAtom, SidecarHeadsScope, SpiiceBundle,
    SpiiceContract, SpiiceRegistry, Surface, TransferRule, sidecar_heads_by_key,
};
use proxiima::{AuthPath, AuthzContext, EntityId, FactWrite, GroupId, OwnerId, Role, UserId};
use proxiima_example::ExampleSpiice;
use proxiima_storage_pg::{PgPlatformScope, PgStorage, PgTuning};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

macro_rules! observation {
    ($payload:ident, $row:ident, $schema:literal, $table:literal, $keys:expr) => {
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
        struct $payload {
            key: String,
            companion: Uuid,
            body: String,
        }

        impl $payload {
            fn fixture(key: &str, companion: Uuid, body: &str) -> Self {
                Self {
                    key: key.into(),
                    companion,
                    body: body.into(),
                }
            }
        }

        impl FactPayload for $payload {
            const SCHEMA_ID: &'static str = $schema;
            const SCHEMA_VERSION: u32 = 1;

            fn receipt_key(&self) -> Vec<u8> {
                let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
                key.field_str("key", &self.key);
                key.field_uuid("companion", self.companion);
                key.field_str("body", &self.body);
                key.finish()
            }

            fn render(&self) -> String {
                self.body.clone()
            }
            fn sidecar_table() -> Option<&'static str> {
                Some($table)
            }
            fn natural_key_columns() -> &'static [&'static str] {
                $keys
            }
        }

        proxiima::spiice::pg_sidecar! {
            payload: $payload,
            row: $row,
            kinds: [Fact],
            table: $table,
            key: admission_id,
            fields: {
                key => key: (text),
                companion => companion: (uuid),
                body => body: (text),
            },
        }
    };
}

observation!(
    TextObservation,
    TextRow,
    "automatic-key/text-v1",
    "automatic_key.text_v1",
    &["key"]
);
observation!(
    CompositeObservation,
    CompositeRow,
    "automatic-key/composite-v1",
    "automatic_key.composite_v1",
    &["key", "companion"]
);

macro_rules! schema {
    ($name:literal, $table:literal, $constraint:literal, $keys:expr) => {
        SchemaContract {
            id: SchemaRef::new("automatic-key", $name, 1),
            kind: PayloadKind::Fact,
            sidecar_table: Some($table),
            search: SearchProjectionDecl::None {
                why: "the fixture tests automatic series selection",
                tag_column: None,
            },
            embedding: EmbeddingRecipe::Never {
                why: "the fixture has no semantic retrieval",
            },
            transfer: TransferRule::StaysOnKey,
            provenance: Provenance::None,
            surfaces: &[Surface {
                table: $table,
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
                    relation: $table,
                    name: $constraint,
                }),
            }],
            natural_key_columns: $keys,
        }
    };
}

static CONTRACT: SpiiceContract = SpiiceContract {
    retired_facts: &[],
    spiice_id: "automatic-key",
    ordinal: 80,
    schemas: &[
        schema!(
            "text",
            "automatic_key.text_v1",
            "text_v1_admission_id_fkey",
            &["key"]
        ),
        schema!(
            "composite",
            "automatic_key.composite_v1",
            "composite_v1_admission_id_fkey",
            &["key", "companion"]
        ),
    ],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None {
        why: "the fixture tests automatic series selection",
    },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

proxiima_core::proxiima_spiice! {
    name = "automatic-key",
    fact_schemas = [TextObservation, CompositeObservation],
    contract = &CONTRACT,
}

async fn fixture(
    prefix: &str,
) -> Result<example_engine::ExampleEngine, Box<dyn std::error::Error>> {
    let db = example_engine::migrated_db(prefix).await;
    example_engine::initialize_store_number(&db).await?;
    let mut registry = SpiiceRegistry::new();
    ExampleSpiice::register(&mut registry)?;
    register(&mut registry)?;
    let registry = registry.try_freeze()?;
    let mut sidecars = PgSidecarRegistry::new();
    proxiima::spiice::register_core_pg_sidecars(&mut sidecars);
    ExampleSpiice::register_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<TextObservation>();
    sidecars.add_fact::<CompositeObservation>();
    let sidecars = sidecars.freeze_against(&registry)?;
    let admin = example_engine::fixture_pool(&db.admin_url(), 2).await?;
    sqlx::raw_sql(
        "CREATE SCHEMA automatic_key;
         CREATE TABLE automatic_key.text_v1 (
             admission_id uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             key text NOT NULL, companion uuid NOT NULL, body text NOT NULL);
         CREATE TABLE automatic_key.composite_v1 (
             admission_id uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             key text NOT NULL, companion uuid NOT NULL, body text NOT NULL);
         ALTER SCHEMA automatic_key OWNER TO proxiima_test_platform;
         ALTER TABLE automatic_key.text_v1 OWNER TO proxiima_test_platform;
         ALTER TABLE automatic_key.composite_v1 OWNER TO proxiima_test_platform;
         INSERT INTO proxiima_core.spiice_surface(table_name,spiice_id)
             VALUES ('automatic_key.text_v1','automatic-key'), ('automatic_key.composite_v1','automatic-key');
         SELECT proxiima_core.install_owner_rls('automatic_key', ARRAY[]::text[],
             ARRAY['text_v1','composite_v1']::text[], ARRAY[]::text[]);
         GRANT USAGE ON SCHEMA automatic_key TO proxiima_test_runtime;
         GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA automatic_key TO proxiima_test_runtime;",
    ).execute(&admin).await?;
    let mut triggers = sidecars.declaration_trigger_artifacts("automatic-key")?;
    triggers.extend(sidecars.presence_trigger_artifacts("automatic-key")?);
    for artifact in triggers {
        // SQL-POLICY: generated — the frozen fixture registry owns the trigger identifiers.
        sqlx::raw_sql(sqlx::AssertSqlSafe(artifact.forward))
            .execute(&admin)
            .await?;
    }
    let platform =
        example_engine::fixture_pool(db.platform_url(), example_engine::ENGINE_POOL_CONNECTIONS)
            .await?;
    let platform = PgPlatformScope::new(
        platform,
        &["proxiima_core", "proxiima_example", "automatic_key"],
    )
    .await?;
    let storage = PgStorage::connect_with_config(
        db.runtime_url(),
        example_engine::engine_pool_config(),
        PgTuning::default(),
        platform,
        0,
    )
    .await?
    .with_sidecars(sidecars)
    .with_spiices(&registry);
    let engine =
        proxiima::Engine::new(registry).with_storage_ports(Arc::new(storage).storage_ports());
    Ok(example_engine::ExampleEngine {
        engine,
        pool: admin,
        db,
    })
}

fn authz(subject: UserId, groups: &[GroupId]) -> AuthzContext {
    proxiima_core::test_fixtures::authenticated_context(AuthzContext::for_subject_with_role(
        subject,
        groups
            .iter()
            .map(|group| (OwnerId::from(*group), Role::admin())),
        AuthPath::HostBearer,
    ))
}

#[allow(clippy::too_many_lines)]
async fn transfer_and_reuse<P: FactPayload + Clone>(
    fixture: &example_engine::ExampleEngine,
    payload: impl Fn(&str) -> P,
    expected_key: &[(String, SidecarAtom)],
) -> TestResult {
    let author = UserId::new(Uuid::now_v7());
    let source = OwnerId::from(author);
    let destination_group = GroupId::new(Uuid::now_v7());
    let destination = OwnerId::from(destination_group);
    sqlx::query("INSERT INTO proxiima_core.owners(owner_id, kind) VALUES ($1, 'group')")
        .bind(destination.into_inner())
        .execute(&fixture.pool)
        .await?;
    let both = authz(author, &[destination_group]);
    let source_only = authz(author, &[]);
    let engine = &fixture.engine;
    let first_payload = payload("first");
    let next_payload = payload("uncommitted update");
    let mut unit = engine.unit_of_work(&both).await?;
    let first = unit
        .ingest_fact(FactWrite::new(source, "automatic-key", &first_payload))
        .await?;
    let next = unit
        .ingest_fact(FactWrite::new(source, "automatic-key", &next_payload))
        .await?;
    assert_eq!(
        first.handle, next.handle,
        "the resolver sees an uncommitted earlier write"
    );
    assert_ne!(first.memory_id, next.memory_id);
    unit.commit().await?;
    engine
        .transfer_to_owner(&both, EntityId::Memory(next.memory_id), destination)
        .await?;
    let transferred_payload = payload("destination update");
    let mut unit = engine.unit_of_work(&both).await?;
    let transferred = unit
        .ingest_fact(FactWrite::new(
            destination,
            "automatic-key",
            &transferred_payload,
        ))
        .await?;
    assert_eq!(
        first.handle, transferred.handle,
        "the transferred series keeps its natural identity"
    );
    assert_ne!(next.memory_id, transferred.memory_id);
    let destination_next_payload = payload("uncommitted destination update");
    let destination_next = unit
        .ingest_fact(FactWrite::new(
            destination,
            "automatic-key",
            &destination_next_payload,
        ))
        .await?;
    assert_eq!(transferred.handle, destination_next.handle);
    assert_ne!(transferred.memory_id, destination_next.memory_id);
    unit.commit().await?;
    let denied_payload = payload("former owner attempts explicit handle");
    let denied = engine
        .ingest_fact(
            &source_only,
            FactWrite::new(source, "automatic-key", &denied_payload)
                .handle(proxiima::SeriesHandle::new(first.handle)),
        )
        .await;
    assert!(
        denied.is_err(),
        "an explicit handle cannot recover transferred authority"
    );
    let vacant_payload = payload("source reuses vacant key");
    let reused = engine
        .ingest_fact(
            &source_only,
            FactWrite::new(source, "automatic-key", &vacant_payload),
        )
        .await?;
    assert_ne!(
        first.handle, reused.handle,
        "a vacant owner-local key starts another series"
    );
    let last_payload = payload("source update");
    let last = engine
        .ingest_fact(
            &source_only,
            FactWrite::new(source, "automatic-key", &last_payload),
        )
        .await?;
    assert_eq!(reused.handle, last.handle);
    for (owner, head) in [
        (source, last.memory_id),
        (destination, destination_next.memory_id),
    ] {
        let page = sidecar_heads_by_key::<P>(
            engine,
            &both,
            SidecarHeadsScope::Owner(owner),
            &[],
            None,
            10,
        )
        .await?;
        assert_eq!(page.heads.len(), 1);
        assert_eq!(page.heads[0].memory_id, head);
        assert_eq!(page.heads[0].natural_key, expected_key);
    }
    Ok(())
}

#[tokio::test]
async fn uuid_shaped_text_keeps_its_series_across_transfer_and_vacant_key_reuse() -> TestResult {
    let fixture = fixture("automatic_text_key").await?;
    let key = Uuid::now_v7().to_string();
    let companion = Uuid::now_v7();
    transfer_and_reuse(
        &fixture,
        |body| TextObservation::fixture(&key, companion, body),
        &[("key".into(), SidecarAtom::Text(key.clone()))],
    )
    .await
}

#[tokio::test]
async fn composite_text_uuid_key_keeps_declared_atoms_and_transaction_local_identity() -> TestResult
{
    let fixture = fixture("automatic_composite_key").await?;
    let companion = Uuid::now_v7();
    let key = companion.to_string();
    transfer_and_reuse(
        &fixture,
        |body| CompositeObservation::fixture(&key, companion, body),
        &[
            ("key".into(), SidecarAtom::Text(key.clone())),
            ("companion".into(), SidecarAtom::Uuid(companion)),
        ],
    )
    .await
}
