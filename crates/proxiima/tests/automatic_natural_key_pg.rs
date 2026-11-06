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
            #[serde(rename = "wire_key")]
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
        }

        impl proxiima::spiice::PayloadProjection for $payload {
            const KEY_FIELDS: &'static [proxiima::spiice::KeyField<Self>] = $keys;

            fn display(&self) -> proxiima::spiice::PayloadDisplay {
                proxiima::spiice::PayloadDisplay {
                    body: Some(self.body.clone()),
                    ..Default::default()
                }
            }

            fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
                let mut builder = proxiima::spiice::DurablePayloadBuilder::new();
                builder
                    .field("key", &self.key)?
                    .field("companion", &self.companion)?;
                builder.field("body", &self.body)?;
                Ok(builder.finish())
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

macro_rules! scalar_observation {
    ($payload:ident, $row:ident, $schema:literal, $table:literal,
        $key_ty:ty, $sidecar_kind:tt, $builder_method:ident,
        $key_field_ctor:ident, $getter:expr, $key_value:expr) => {
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
        struct $payload {
            key: $key_ty,
            companion: Uuid,
            body: String,
        }

        impl $payload {
            fn fixture(key: $key_ty, companion: Uuid, body: &str) -> Self {
                Self {
                    key,
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
                key.$builder_method("key", $key_value(self));
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
        }

        impl proxiima::spiice::PayloadProjection for $payload {
            const KEY_FIELDS: &'static [proxiima::spiice::KeyField<Self>] =
                &[proxiima::spiice::KeyField::$key_field_ctor("key", $getter)];

            fn display(&self) -> proxiima::spiice::PayloadDisplay {
                proxiima::spiice::PayloadDisplay {
                    body: Some(self.body.clone()),
                    ..Default::default()
                }
            }

            fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
                let mut builder = proxiima::spiice::DurablePayloadBuilder::new();
                builder
                    .field("key", &self.key)?
                    .field("companion", &self.companion)?;
                builder.field("body", &self.body)?;
                Ok(builder.finish())
            }
        }

        proxiima::spiice::pg_sidecar! {
            payload: $payload,
            row: $row,
            kinds: [Fact],
            table: $table,
            key: admission_id,
            fields: {
                key => key: ($sidecar_kind),
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
    &[proxiima::spiice::KeyField::text("key", |payload| &payload.key)]
);

scalar_observation!(
    UuidObservation,
    UuidRow,
    "automatic-key/uuid-v1",
    "automatic_key.uuid_v1",
    Uuid,
    uuid,
    field_uuid,
    uuid,
    |payload| payload.key,
    |payload: &UuidObservation| payload.key
);

fn char_key(payload: &CharObservation) -> &str {
    payload.key.as_str()
}

scalar_observation!(
    BoolObservation,
    BoolRow,
    "automatic-key/bool-v1",
    "automatic_key.bool_v1",
    bool,
    bool,
    field_bool,
    bool,
    |payload| payload.key,
    |payload: &BoolObservation| payload.key
);
scalar_observation!(
    Int2Observation,
    Int2Row,
    "automatic-key/int2-v1",
    "automatic_key.int2_v1",
    i16,
    i16,
    field_i32,
    i16,
    |payload| payload.key,
    |payload: &Int2Observation| i32::from(payload.key)
);
scalar_observation!(
    Int4Observation,
    Int4Row,
    "automatic-key/int4-v1",
    "automatic_key.int4_v1",
    i32,
    i32,
    field_i32,
    i32,
    |payload| payload.key,
    |payload: &Int4Observation| payload.key
);
scalar_observation!(
    Int8Observation,
    Int8Row,
    "automatic-key/int8-v1",
    "automatic_key.int8_v1",
    i64,
    i64,
    field_i64,
    i64,
    |payload| payload.key,
    |payload: &Int8Observation| payload.key
);
scalar_observation!(
    CharObservation,
    CharRow,
    "automatic-key/char-v1",
    "automatic_key.char_v1",
    String,
    text,
    field_str,
    padded_text,
    char_key,
    char_key
);
observation!(
    CompositeObservation,
    CompositeRow,
    "automatic-key/composite-v1",
    "automatic_key.composite_v1",
    &[
        proxiima::spiice::KeyField::text("key", |payload| &payload.key),
        proxiima::spiice::KeyField::uuid("companion", |payload| payload.companion),
    ]
);

macro_rules! schema {
    ($name:literal, $table:literal, $constraint:literal, $payload:ty) => {
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
            natural_key_columns: proxiima::spiice::NaturalKeyDeclaration::of::<$payload>(),
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
            TextObservation
        ),
        schema!(
            "composite",
            "automatic_key.composite_v1",
            "composite_v1_admission_id_fkey",
            CompositeObservation
        ),
        schema!(
            "uuid",
            "automatic_key.uuid_v1",
            "uuid_v1_admission_id_fkey",
            UuidObservation
        ),
        schema!(
            "bool",
            "automatic_key.bool_v1",
            "bool_v1_admission_id_fkey",
            BoolObservation
        ),
        schema!(
            "int2",
            "automatic_key.int2_v1",
            "int2_v1_admission_id_fkey",
            Int2Observation
        ),
        schema!(
            "int4",
            "automatic_key.int4_v1",
            "int4_v1_admission_id_fkey",
            Int4Observation
        ),
        schema!(
            "int8",
            "automatic_key.int8_v1",
            "int8_v1_admission_id_fkey",
            Int8Observation
        ),
        schema!(
            "char",
            "automatic_key.char_v1",
            "char_v1_admission_id_fkey",
            CharObservation
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
    evolutions: &[],
};

proxiima_core::proxiima_spiice! {
    name = "automatic-key",
    fact_schemas = [
        TextObservation, CompositeObservation, UuidObservation, BoolObservation,
        Int2Observation, Int4Observation, Int8Observation, CharObservation,
    ],
    contract = &CONTRACT,
}

fn registries() -> Result<
    (
        proxiima::spiice::SpiiceRegistryFrozen,
        proxiima_storage_pg::PgSidecarRegistryFrozen,
    ),
    Box<dyn std::error::Error>,
> {
    let mut registry = SpiiceRegistry::new();
    ExampleSpiice::register(&mut registry)?;
    register(&mut registry)?;
    let registry = registry.try_freeze()?;
    let mut sidecars = PgSidecarRegistry::new();
    proxiima::spiice::register_core_pg_sidecars(&mut sidecars);
    ExampleSpiice::register_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<TextObservation>();
    sidecars.add_fact::<CompositeObservation>();
    sidecars.add_fact::<UuidObservation>();
    sidecars.add_fact::<BoolObservation>();
    sidecars.add_fact::<Int2Observation>();
    sidecars.add_fact::<Int4Observation>();
    sidecars.add_fact::<Int8Observation>();
    sidecars.add_fact::<CharObservation>();
    let sidecars = sidecars.freeze_against(&registry)?;
    Ok((registry, sidecars))
}

async fn connect_storage(
    db: &proxiima::testkit::SplitRoleDb,
) -> Result<PgStorage, Box<dyn std::error::Error>> {
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
    .await?;
    Ok(storage)
}

async fn fixture(
    prefix: &str,
) -> Result<example_engine::ExampleEngine, Box<dyn std::error::Error>> {
    let db = example_engine::migrated_db(prefix).await;
    example_engine::initialize_store_number(&db).await?;
    let (registry, sidecars) = registries()?;
    let admin = example_engine::fixture_pool(&db.admin_url(), 2).await?;
    sqlx::raw_sql(
        "CREATE SCHEMA automatic_key;
         CREATE TABLE automatic_key.text_v1 (
             admission_id uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             key text NOT NULL, companion uuid NOT NULL, body text NOT NULL);
         CREATE TABLE automatic_key.composite_v1 (
             admission_id uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             key text NOT NULL, companion uuid NOT NULL, body text NOT NULL);
         CREATE TABLE automatic_key.uuid_v1 (
             admission_id uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             key uuid NOT NULL, companion uuid NOT NULL, body text NOT NULL);
         CREATE TABLE automatic_key.bool_v1 (
             admission_id uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             key boolean NOT NULL, companion uuid NOT NULL, body text NOT NULL);
         CREATE TABLE automatic_key.int2_v1 (
             admission_id uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             key smallint NOT NULL, companion uuid NOT NULL, body text NOT NULL);
         CREATE TABLE automatic_key.int4_v1 (
             admission_id uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             key integer NOT NULL, companion uuid NOT NULL, body text NOT NULL);
         CREATE TABLE automatic_key.int8_v1 (
             admission_id uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             key bigint NOT NULL, companion uuid NOT NULL, body text NOT NULL);
         CREATE TABLE automatic_key.char_v1 (
             admission_id uuid PRIMARY KEY REFERENCES proxiima_core.memory(t),
             key char(64) NOT NULL, companion uuid NOT NULL, body text NOT NULL);
         ALTER SCHEMA automatic_key OWNER TO proxiima_test_platform;
         ALTER TABLE automatic_key.text_v1 OWNER TO proxiima_test_platform;
         ALTER TABLE automatic_key.composite_v1 OWNER TO proxiima_test_platform;
         ALTER TABLE automatic_key.uuid_v1 OWNER TO proxiima_test_platform;
         ALTER TABLE automatic_key.bool_v1 OWNER TO proxiima_test_platform;
         ALTER TABLE automatic_key.int2_v1 OWNER TO proxiima_test_platform;
         ALTER TABLE automatic_key.int4_v1 OWNER TO proxiima_test_platform;
         ALTER TABLE automatic_key.int8_v1 OWNER TO proxiima_test_platform;
         ALTER TABLE automatic_key.char_v1 OWNER TO proxiima_test_platform;
         INSERT INTO proxiima_core.spiice_surface(table_name,spiice_id)
             VALUES ('automatic_key.text_v1','automatic-key'), ('automatic_key.composite_v1','automatic-key'),
                    ('automatic_key.uuid_v1','automatic-key'), ('automatic_key.bool_v1','automatic-key'),
                    ('automatic_key.int2_v1','automatic-key'), ('automatic_key.int4_v1','automatic-key'),
                    ('automatic_key.int8_v1','automatic-key'), ('automatic_key.char_v1','automatic-key');
         SELECT proxiima_core.install_owner_rls('automatic_key', ARRAY[]::text[],
             ARRAY['text_v1','composite_v1','uuid_v1','bool_v1','int2_v1','int4_v1','int8_v1','char_v1']::text[], ARRAY[]::text[]);
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
    let storage = connect_storage(&db)
        .await?
        .with_sidecars(sidecars)
        .await?
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

async fn assert_actual_key<P: FactPayload + Clone>(
    fixture: &example_engine::ExampleEngine,
    first_payload: P,
    second_payload: P,
    expected_key: &[(String, SidecarAtom)],
) -> TestResult {
    let subject = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(subject);
    let authz = authz(subject, &[]);
    let mut unit = fixture.engine.unit_of_work(&authz).await?;
    let first = unit
        .ingest_fact(FactWrite::new(owner, "automatic-key", &first_payload))
        .await?;
    let second = unit
        .ingest_fact(FactWrite::new(owner, "automatic-key", &second_payload))
        .await?;
    assert_eq!(
        first.handle, second.handle,
        "natural key reuses transaction-local series"
    );
    unit.commit().await?;

    let page = sidecar_heads_by_key::<P>(
        &fixture.engine,
        &authz,
        SidecarHeadsScope::Owner(owner),
        &[],
        None,
        10,
    )
    .await?;
    assert_eq!(page.heads.len(), 1);
    assert_eq!(page.heads[0].memory_id, second.memory_id);
    assert_eq!(page.heads[0].natural_key, expected_key);
    let prefix = expected_key
        .iter()
        .map(|(_, value)| value.clone())
        .collect::<Vec<_>>();
    let filtered = sidecar_heads_by_key::<P>(
        &fixture.engine,
        &authz,
        SidecarHeadsScope::Owner(owner),
        &prefix,
        None,
        10,
    )
    .await?;
    assert_eq!(filtered.heads, page.heads);
    Ok(())
}

#[tokio::test]
async fn raw_stateful_fact_cannot_bypass_typed_key_binding() -> TestResult {
    let fixture = fixture("automatic_raw_refusal").await?;
    let subject = UserId::new(Uuid::now_v7());
    let authz = authz(subject, &[]);
    let payload = TextObservation::fixture("raw-key", Uuid::now_v7(), "body");
    let command = proxiima_core::verbs::fact_ingest::FactWriteCommand::from_payload(
        "automatic-key",
        &payload,
    );
    let mut missing_key = proxiima_core::SidecarPayload::fact(proxiima_example::StampV1 {
        mark: "not a declared text key".into(),
    });
    missing_key.schema_id = TextObservation::schema_id();
    let mut foreign_key = proxiima_core::SidecarPayload::fact(CompositeObservation::fixture(
        "other key",
        Uuid::now_v7(),
        "other body",
    ));
    foreign_key.schema_id = TextObservation::schema_id();
    for sidecars in [vec![], vec![missing_key], vec![foreign_key]] {
        let error = fixture
            .engine
            .authorize_fact_ingest(&authz, command.clone(), &sidecars)
            .await
            .expect_err("an unbound or foreign key must not receive a witness");
        assert_eq!(error.code, proxiima_core::error::ErrorCode::InvalidArgument);
    }
    let error = fixture
        .engine
        .fact_ingest(&authz, command)
        .await
        .expect_err("a raw command cannot mint an unbound stateful series");
    assert_eq!(error.code, proxiima_core::error::ErrorCode::InvalidArgument);
    let admitted: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxiima_core.memory WHERE schema_id = $1")
            .bind(TextObservation::SCHEMA_ID)
            .fetch_one(&fixture.pool)
            .await?;
    assert_eq!(admitted, 0, "refusal must precede any admission");
    Ok(())
}

#[tokio::test]
async fn actual_uuid_natural_key_reuses_series() -> TestResult {
    let fixture = fixture("automatic_uuid_key").await?;
    let key = Uuid::now_v7();
    assert_actual_key(
        &fixture,
        UuidObservation::fixture(key, Uuid::now_v7(), "first"),
        UuidObservation::fixture(key, Uuid::now_v7(), "second"),
        &[("key".into(), SidecarAtom::Uuid(key))],
    )
    .await
}

#[tokio::test]
async fn actual_bool_natural_key_reuses_series() -> TestResult {
    let fixture = fixture("automatic_bool_key").await?;
    assert_actual_key(
        &fixture,
        BoolObservation::fixture(true, Uuid::now_v7(), "first"),
        BoolObservation::fixture(true, Uuid::now_v7(), "second"),
        &[("key".into(), SidecarAtom::Bool(true))],
    )
    .await
}

#[tokio::test]
async fn actual_int2_natural_key_reuses_series() -> TestResult {
    let fixture = fixture("automatic_int2_key").await?;
    assert_actual_key(
        &fixture,
        Int2Observation::fixture(17, Uuid::now_v7(), "first"),
        Int2Observation::fixture(17, Uuid::now_v7(), "second"),
        &[("key".into(), SidecarAtom::I32(17))],
    )
    .await
}

#[tokio::test]
async fn actual_int4_natural_key_reuses_series() -> TestResult {
    let fixture = fixture("automatic_int4_key").await?;
    assert_actual_key(
        &fixture,
        Int4Observation::fixture(1_700_000, Uuid::now_v7(), "first"),
        Int4Observation::fixture(1_700_000, Uuid::now_v7(), "second"),
        &[("key".into(), SidecarAtom::I32(1_700_000))],
    )
    .await
}

#[tokio::test]
async fn actual_int8_natural_key_reuses_series() -> TestResult {
    let fixture = fixture("automatic_int8_key").await?;
    assert_actual_key(
        &fixture,
        Int8Observation::fixture(4_000_000_000, Uuid::now_v7(), "first"),
        Int8Observation::fixture(4_000_000_000, Uuid::now_v7(), "second"),
        &[("key".into(), SidecarAtom::I64(4_000_000_000))],
    )
    .await
}

#[tokio::test]
async fn actual_char_natural_key_canonicalizes_padding() -> TestResult {
    let fixture = fixture("automatic_char_key").await?;
    assert_actual_key(
        &fixture,
        CharObservation::fixture("x".into(), Uuid::now_v7(), "first"),
        CharObservation::fixture("x ".into(), Uuid::now_v7(), "second"),
        &[("key".into(), SidecarAtom::Text("x".into()))],
    )
    .await
}

#[tokio::test]
async fn schema_validation_refuses_incompatible_columns() -> TestResult {
    let fixture = fixture("automatic_key_validation").await?;
    let storage = connect_storage(&fixture.db).await?;
    let (_, sidecars) = registries()?;

    sqlx::query("ALTER TABLE automatic_key.text_v1 ALTER COLUMN key TYPE char(64)")
        .execute(&fixture.pool)
        .await?;
    let char_error = storage
        .clone()
        .with_sidecars(sidecars.clone())
        .await
        .expect_err("text declarations must refuse CHAR columns");
    assert!(char_error.to_string().contains("incompatible"));

    sqlx::query("ALTER TABLE automatic_key.text_v1 ALTER COLUMN key TYPE text")
        .execute(&fixture.pool)
        .await?;
    sqlx::query("ALTER TABLE automatic_key.text_v1 ALTER COLUMN key DROP NOT NULL")
        .execute(&fixture.pool)
        .await?;
    let nullable_error = storage
        .clone()
        .with_sidecars(sidecars.clone())
        .await
        .expect_err("natural-key declarations must refuse nullable columns");
    assert!(nullable_error.to_string().contains("nullable"));

    sqlx::query("ALTER TABLE automatic_key.text_v1 ALTER COLUMN key SET NOT NULL")
        .execute(&fixture.pool)
        .await?;
    sqlx::query("CREATE COLLATION automatic_key.casefold (provider = icu, locale = 'und-u-ks-level2', deterministic = false)")
        .execute(&fixture.pool)
        .await?;
    sqlx::query("ALTER TABLE automatic_key.text_v1 ALTER COLUMN key TYPE text COLLATE automatic_key.casefold")
        .execute(&fixture.pool)
        .await?;
    let collation_error = storage
        .clone()
        .with_sidecars(sidecars.clone())
        .await
        .expect_err("byte-based locks cannot support nondeterministic SQL equality");
    assert!(collation_error.to_string().contains("nondeterministic"));

    sqlx::query("ALTER TABLE automatic_key.text_v1 RENAME COLUMN key TO missing_key")
        .execute(&fixture.pool)
        .await?;
    let missing_error = storage
        .clone()
        .with_sidecars(sidecars.clone())
        .await
        .expect_err("missing key columns must fail attachment");
    assert!(missing_error.to_string().contains("missing"));
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
