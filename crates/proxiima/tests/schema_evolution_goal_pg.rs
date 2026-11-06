//! End-to-end Goal representation evolution receipt.

#[path = "schema_evolution_goal_pg/replacement.rs"]
mod replacement;
#[path = "fixtures/split_core_db.rs"]
mod split_core_db;

use proxiima::GoalModifyRequest;
use proxiima::spiice::{
    GoalPayload, GoalPayloadWrite, MigrationPlacement, NamedMigrator, PayloadKeyBuilder,
    PgGoalSidecar, PgMigrationGoalPayload, PgMigrationSchemas, PgSchemaEvolution, PgSidecarReadCtx,
    SpiiceBundle, SpiiceDescriptor, SpiiceProvenance, SpiiceRegistry, SpiiceRegistryError,
};
use proxiima_core::publication::{PublicationConfig, PublicationSource};
use proxiima_core::spiice::{
    CounterRule, DbConstraint, EmbeddingRecipe, EraseRule, ExportRule, ForgetRule, KeyShape,
    ProjectionDecl, Provenance, SchemaContract, SchemaEvolution, SchemaRef, SearchProjectionDecl,
    SpiiceContract, Surface, TransferRule,
};
use proxiima_core::test_fixtures::authenticated_context;
use proxiima_core::verbs::goal_write::{
    GoalAuthorship, GoalCreateRequest, GoalTopologyWrite, GoalTransitionKind, GoalWriteOutcome,
    IdempotencyKey,
};
use proxiima_core::{AuthPath, AuthzContext, GoalId, OwnerId, PendingWork, Role, UserId};
use proxiima_storage_pg::{
    PgMigrationStorage, PgPlatformScope, PgPoolConfig, PgSidecarRegistry, PgStorage, PgTuning,
    register_core_pg_sidecars,
};
use sqlx::SqlSafeStr;
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::{FromRow, Postgres, Row};
use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use uuid::Uuid;

const SPIICE: &str = "evolution-goal-test";
const V1_ID: &str = "evolution-goal-test/goal-v1";
const V2_ID: &str = "evolution-goal-test/goal-v2";
const V1_TABLE: &str = "evolution_goal_test.goal_v1";
const V2_TABLE: &str = "evolution_goal_test.goal_v2";
const ANCHOR: i64 = 2_026_110_600_101;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct GoalV1 {
    key: String,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct GoalV2 {
    key: String,
    label: String,
}

macro_rules! goal_payload {
    ($ty:ty, $id:expr, $version:expr, $table:literal) => {
        impl GoalPayload for $ty {
            const SCHEMA_ID: &'static str = $id;
            const SCHEMA_VERSION: u32 = $version;
            fn sidecar_table() -> Option<&'static str> {
                Some($table)
            }
            fn goal_key(&self) -> Vec<u8> {
                let mut b = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
                b.field_str("key", &self.key);
                b.finish()
            }
        }
        impl proxiima::spiice::PayloadProjection for $ty {
            const KEY_FIELDS: &'static [proxiima::spiice::KeyField<Self>] = &[];
            fn display(&self) -> proxiima::spiice::PayloadDisplay {
                proxiima::spiice::PayloadDisplay {
                    body: Some(self.key.clone()),
                    ..Default::default()
                }
            }
            fn durable(&self) -> Result<proxiima::spiice::DurablePayload, String> {
                let mut b = proxiima::spiice::DurablePayloadBuilder::new();
                b.field("key", &self.key)?;
                Ok(b.finish())
            }
        }
    };
}
goal_payload!(GoalV1, V1_ID, 1, "evolution_goal_test.goal_v1");
goal_payload!(GoalV2, V2_ID, 2, "evolution_goal_test.goal_v2");

#[derive(Debug)]
struct GoalRow {
    key: String,
}
impl<'r> FromRow<'r, sqlx::postgres::PgRow> for GoalRow {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            key: row.try_get("key")?,
        })
    }
}

macro_rules! goal_sidecar {
    ($ty:ty, $table:literal) => {
        impl PgGoalSidecar for $ty {
            fn insert_goal_sidecar<'t>(
                &'t self,
                tx: &'t mut sqlx::Transaction<'_, Postgres>,
                id: GoalId,
            ) -> proxiima::spiice::PgSidecarFuture<'t> {
                Box::pin(async move {
                    sqlx::query(concat!("INSERT INTO ", $table, " (t,key) VALUES ($1,$2)"))
                        .bind(id.into_inner())
                        .bind(&self.key)
                        .execute(tx.as_mut())
                        .await
                        .map_err(proxiima_storage_pg::map_err)?;
                    Ok(())
                })
            }
            fn copy_goal_sidecar<'t>(
                tx: &'t mut sqlx::Transaction<'_, Postgres>,
                id: GoalId,
                source: GoalId,
            ) -> proxiima::spiice::PgSidecarFuture<'t> {
                Box::pin(async move {
                    sqlx::query(concat!(
                        "INSERT INTO ",
                        $table,
                        " (t,key) SELECT $1,key FROM ",
                        $table,
                        " WHERE t=$2"
                    ))
                    .bind(id.into_inner())
                    .bind(source.into_inner())
                    .execute(tx.as_mut())
                    .await
                    .map_err(proxiima_storage_pg::map_err)?;
                    Ok(())
                })
            }
        }
    };
}
goal_sidecar!(GoalV1, "evolution_goal_test.goal_v1");
goal_sidecar!(GoalV2, "evolution_goal_test.goal_v2");

impl PgMigrationGoalPayload for GoalV1 {
    fn load<'a>(
        mut ctx: PgSidecarReadCtx<'a>,
        ids: &'a [GoalId],
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Vec<(GoalId, GoalV1)>, proxiima_core::StorageError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let mut out = Vec::new();
            for id in ids {
                if let Some(row) = ctx
                    .reborrow()
                    .fetch_optional_by_memory_id::<GoalRow>(
                        "SELECT t,key FROM evolution_goal_test.goal_v1 WHERE t=$1",
                        proxiima_core::MemoryId::new(id.into_inner()),
                    )
                    .await?
                {
                    out.push((*id, GoalV1 { key: row.key }));
                }
            }
            Ok(out)
        })
    }
}

static SURFACE_V1: Surface = Surface {
    table: V1_TABLE,
    key: KeyShape::GoalT { column: "t" },
    owner_column: None,
    transfer: TransferRule::StaysOnKey,
    erase: EraseRule::ByKey,
    export: ExportRule::Rows,
    forget: ForgetRule::DeleteWithMemory,
    lexical_language_column: None,
    counter: CounterRule::Counted("sidecar_rows"),
    completeness: Some(DbConstraint {
        relation: V1_TABLE,
        name: "goal_v1_t_fkey",
    }),
};
static SURFACE_V2: Surface = Surface {
    table: V2_TABLE,
    key: KeyShape::GoalT { column: "t" },
    owner_column: None,
    transfer: TransferRule::StaysOnKey,
    erase: EraseRule::ByKey,
    export: ExportRule::Rows,
    forget: ForgetRule::DeleteWithMemory,
    lexical_language_column: None,
    counter: CounterRule::Counted("sidecar_rows"),
    completeness: Some(DbConstraint {
        relation: V2_TABLE,
        name: "goal_v2_t_fkey",
    }),
};
static C1: SchemaContract = SchemaContract {
    id: SchemaRef::new(SPIICE, "goal", 1),
    kind: proxiima_core::verbs::schema::PayloadKind::Goal,
    sidecar_table: Some(V1_TABLE),
    search: SearchProjectionDecl::None {
        why: "receipt",
        tag_column: None,
    },
    embedding: EmbeddingRecipe::Never { why: "receipt" },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[SURFACE_V1],
    natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
};
static C2: SchemaContract = SchemaContract {
    id: SchemaRef::new(SPIICE, "goal", 2),
    kind: proxiima_core::verbs::schema::PayloadKind::Goal,
    sidecar_table: Some(V2_TABLE),
    search: SearchProjectionDecl::None {
        why: "receipt",
        tag_column: None,
    },
    embedding: EmbeddingRecipe::Never { why: "receipt" },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[SURFACE_V2],
    natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
};
static EV: SchemaEvolution = SchemaEvolution::new(
    "goal-v1-to-v2",
    &[C1],
    &[C2],
    ANCHOR,
    &[],
    "change goal representation",
    "redeploy v2",
);

static OLD_CONTRACT: SpiiceContract = SpiiceContract {
    spiice_id: SPIICE,
    ordinal: 41,
    schemas: &[C1],
    evolutions: &[],
    retired_facts: &[],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None { why: "receipt" },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};
static NEW_CONTRACT: SpiiceContract = SpiiceContract {
    spiice_id: SPIICE,
    ordinal: 41,
    schemas: &[C2],
    evolutions: &[EV],
    retired_facts: &[],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    projection: ProjectionDecl::None { why: "receipt" },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

fn register(r: &mut SpiiceRegistry, old: bool) -> Result<(), SpiiceRegistryError> {
    r.try_add_spiice(SpiiceDescriptor {
        spiice_id: SPIICE.into(),
        display_name: SPIICE.into(),
        package_version: "0".into(),
        author: None,
        provenance: SpiiceProvenance::Builtin,
    })?;
    if old {
        r.try_add_goal_schema::<GoalV1>()?;
        r.try_add_contract(&OLD_CONTRACT)
    } else {
        r.try_add_goal_schema::<GoalV2>()?;
        r.try_add_contract(&NEW_CONTRACT)
    }
}
struct AppOld;
struct AppNew;
impl SpiiceBundle for AppOld {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register(r, true)
    }
    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_goal::<GoalV1>();
    }
    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, migrator())]
    }
}
impl SpiiceBundle for AppNew {
    fn register(r: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        register(r, false)
    }
    fn register_pg_sidecars(r: &mut PgSidecarRegistry) {
        r.add_goal::<GoalV2>();
        let mut m = PgMigrationSchemas::new();
        m.add_goal_source::<GoalV1>();
        m.add_goal::<GoalV2>();
        r.add_evolution(PgSchemaEvolution::new(&EV, m, evolve));
    }
    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new(SPIICE, migrator())]
    }
}
fn migrator() -> Migrator {
    let mut s = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut s);
    s.add_goal::<GoalV1>();
    s.add_goal::<GoalV2>();
    let mut m = PgMigrationSchemas::new();
    m.add_goal_source::<GoalV1>();
    m.add_goal::<GoalV2>();
    s.add_evolution(PgSchemaEvolution::new(&EV, m, evolve));
    let sql = sqlx::AssertSqlSafe(
        "CREATE SCHEMA IF NOT EXISTS evolution_goal_test;
         CREATE TABLE IF NOT EXISTS evolution_goal_test.goal_v1 (
             t uuid PRIMARY KEY REFERENCES proxiima_core.goal(t) ON DELETE CASCADE,
             key text NOT NULL
         );
         CREATE TABLE IF NOT EXISTS evolution_goal_test.goal_v2 (
             t uuid PRIMARY KEY REFERENCES proxiima_core.goal(t) ON DELETE CASCADE,
             key text NOT NULL
         );
         INSERT INTO proxiima_core.spiice_surface(table_name, spiice_id)
             VALUES ('evolution_goal_test.goal_v1','evolution-goal-test'),
                    ('evolution_goal_test.goal_v2','evolution-goal-test')
             ON CONFLICT DO NOTHING;
         SELECT proxiima_core.install_owner_rls(
             'evolution_goal_test', ARRAY[]::text[],
             ARRAY['goal_v1', 'goal_v2']::text[], ARRAY[]::text[]);",
    );
    Migrator {
        migrations: Cow::Owned(vec![Migration::new(
            ANCHOR,
            Cow::Borrowed("goal sidecars"),
            MigrationType::Simple,
            sql.into_sql_str(),
            false,
        )]),
        ..Migrator::DEFAULT
    }
}
fn evolve(
    input: &proxiima::spiice::MigrationInput,
    plan: &mut proxiima::spiice::MigrationPlan,
) -> Result<(), proxiima_core::StorageError> {
    for source in input.goals() {
        let old = source
            .payload::<GoalV1>()
            .ok_or_else(|| proxiima_core::StorageError::Internal("missing GoalV1".into()))?;
        let payload = GoalPayloadWrite::from_payload(
            source.title(),
            source.text().unwrap_or("goal"),
            GoalV2 {
                key: old.key.clone(),
                label: "v2".into(),
            },
        )
        .map_err(|e| proxiima_core::StorageError::Internal(e.to_string()))?;
        let id = plan.goal(source, payload, MigrationPlacement::RETAIN, source.pins())?;
        plan.map_goal(source, id, &[])?;
    }
    Ok(())
}

async fn storage(
    url: &str,
    platform: &str,
    registry: &proxiima_core::SpiiceRegistryFrozen,
    sidecars: PgSidecarRegistry,
) -> PgStorage {
    let pool = sqlx::PgPool::connect(platform).await.unwrap();
    let scope = PgPlatformScope::new(pool, &["proxiima_core", "evolution_goal_test"])
        .await
        .unwrap();
    PgStorage::connect_with_config(
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
    .unwrap()
    .with_sidecars(sidecars.freeze_against(registry).unwrap())
    .await
    .unwrap()
    .with_spiices(registry)
}

async fn create_and_modify_old_goal(
    engine: &proxiima_core::Engine,
    authz: &AuthzContext,
    owner: OwnerId,
    topology: &GoalTopologyWrite,
    request: &IdempotencyKey,
) -> (GoalWriteOutcome, GoalWriteOutcome) {
    let first = engine
        .create_goal(
            authz,
            GoalCreateRequest {
                owner,
                topology: topology.clone(),
                title: "goal".into(),
                text: "body".into(),
                payload: GoalV1 { key: "same".into() },
                request_id: request.clone(),
                authorship: GoalAuthorship::User,
                author_self_perspective_id: None,
            },
        )
        .await
        .unwrap();
    let modified = engine
        .modify_goal(
            authz,
            &GoalModifyRequest {
                owner,
                prior_goal_id: first.goal_id,
                replacement: GoalPayloadWrite::from_payload(
                    "goal 2",
                    "body 2",
                    GoalV1 {
                        key: "same-2".into(),
                    },
                )
                .unwrap(),
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("goal-modify-evolution").unwrap(),
                author_self_perspective_id: None,
                evidence: None,
                motivated_by: None,
            },
        )
        .await
        .unwrap();
    (first, modified)
}

#[tokio::test]
async fn goal_schema_evolution_preserves_ids_series_sidecars_and_replay() {
    let db = split_core_db::clone_split_core_db("proxiima_goal_evolution")
        .await
        .expect("PG required");
    let platform = db.platform_url().to_owned();
    let runtime = db.runtime_url().to_owned();
    let mut ro = SpiiceRegistry::new();
    register(&mut ro, true).unwrap();
    let ro = ro.try_freeze().unwrap();
    let pg = PgMigrationStorage::connect_with_config(
        &platform,
        PgPoolConfig::default(),
        PgTuning::default(),
    )
    .await
    .unwrap();
    let so = {
        let mut s = PgSidecarRegistry::new();
        register_core_pg_sidecars(&mut s);
        s.add_goal::<GoalV1>();
        s
    };
    let fo = so.clone().freeze_against(&ro).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppOld::migrators(), &fo)
        .await
        .unwrap();
    pg.initialize_store_number(0).await.unwrap();
    let owner = OwnerId::new(Uuid::now_v7());
    let user = UserId::new(Uuid::now_v7());
    let authz = authenticated_context(AuthzContext::for_subject_with_role(
        user,
        [(owner, Role::admin())],
        AuthPath::HostBearer,
    ));
    let admin = sqlx::PgPool::connect(&db.admin_url()).await.unwrap();
    insert_owner(&admin, owner).await;
    let old = storage(&runtime, &platform, &ro, so).await;
    let engine = proxiima_core::Engine::new(ro)
        .try_with_publication_config(PublicationConfig::new(
            PublicationSource::new("urn:goal:evolution").unwrap(),
        ))
        .unwrap()
        .with_storage_ports(Arc::new(old).storage_ports());
    let topology = GoalTopologyWrite::new(Vec::new(), Vec::new(), Vec::new()).unwrap();
    let request = IdempotencyKey::new("goal-create-evolution").unwrap();
    let (first, modified) =
        create_and_modify_old_goal(&engine, &authz, owner, &topology, &request).await;
    let mut rn = SpiiceRegistry::new();
    register(&mut rn, false).unwrap();
    let rn = rn.try_freeze().unwrap();
    let sn = {
        let mut s = PgSidecarRegistry::new();
        register_core_pg_sidecars(&mut s);
        s.add_goal::<GoalV2>();
        let mut m = PgMigrationSchemas::new();
        m.add_goal_source::<GoalV1>();
        m.add_goal::<GoalV2>();
        s.add_evolution(PgSchemaEvolution::new(&EV, m, evolve));
        s
    };
    let fn_ = sn.clone().freeze_against(&rn).unwrap();
    proxiima::run_core_and_spiice_migrations(&pg, AppNew::migrators(), &fn_)
        .await
        .unwrap();
    finish_goal_evolution(GoalEvolutionFinish {
        runtime,
        platform,
        rn,
        sn,
        owner,
        authz,
        topology,
        request,
        first,
        modified,
        admin,
    })
    .await;
}

struct GoalEvolutionFinish {
    runtime: String,
    platform: String,
    rn: proxiima_core::SpiiceRegistryFrozen,
    sn: PgSidecarRegistry,
    owner: OwnerId,
    authz: AuthzContext,
    topology: GoalTopologyWrite,
    request: IdempotencyKey,
    first: GoalWriteOutcome,
    modified: GoalWriteOutcome,
    admin: sqlx::PgPool,
}

async fn insert_owner(admin: &sqlx::PgPool, owner: OwnerId) {
    sqlx::query("INSERT INTO proxiima_core.owners(owner_id,kind) VALUES($1,'personal'::proxiima_core.owner_kind)")
        .bind(owner.into_inner())
        .execute(admin)
        .await
        .unwrap();
}

async fn finish_goal_evolution(finish: GoalEvolutionFinish) {
    let GoalEvolutionFinish {
        runtime,
        platform,
        rn,
        sn,
        owner,
        authz,
        topology,
        request,
        first,
        modified,
        admin,
    } = finish;
    let new = storage(runtime.as_str(), platform.as_str(), &rn, sn).await;
    let engine2 = proxiima_core::Engine::new(rn)
        .try_with_publication_config(PublicationConfig::new(
            PublicationSource::new("urn:goal:evolution").unwrap(),
        ))
        .unwrap()
        .with_storage_ports(Arc::new(new).storage_ports());
    let replay = engine2
        .create_goal(
            &authz,
            GoalCreateRequest {
                owner,
                topology,
                title: "goal".into(),
                text: "body".into(),
                payload: GoalV2 {
                    key: "same".into(),
                    label: "v2".into(),
                },
                request_id: request,
                authorship: GoalAuthorship::User,
                author_self_perspective_id: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(replay.goal_id, first.goal_id);
    assert_eq!(modified.goal_handle, first.goal_handle);

    let paused = engine2
        .transition_goal(
            &authz,
            &proxiima_core::GoalTransitionRequest {
                owner,
                prior_goal_id: modified.goal_id,
                transition: GoalTransitionKind::Pause {
                    pending: PendingWork::Keep,
                },
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("goal-pause-after-evolution").unwrap(),
                author_self_perspective_id: None,
            },
        )
        .await
        .unwrap();
    let resumed = engine2
        .transition_goal(
            &authz,
            &proxiima_core::GoalTransitionRequest {
                owner,
                prior_goal_id: paused.goal_id,
                transition: GoalTransitionKind::Resume,
                authorship: GoalAuthorship::User,
                request_id: IdempotencyKey::new("goal-resume-after-evolution").unwrap(),
                author_self_perspective_id: None,
            },
        )
        .await
        .unwrap();
    assert_ne!(paused.goal_id, replay.goal_id);
    assert_ne!(resumed.goal_id, paused.goal_id);
    let ids: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT t,schema_id FROM proxiima_core.goal WHERE owner_id=$1 ORDER BY t")
            .bind(owner.into_inner())
            .fetch_all(&admin)
            .await
            .unwrap();
    assert_eq!(ids.len(), 4);
    assert!(ids.iter().all(|(_, s)| s == V2_ID));
    let old_count: i64 = sqlx::query_scalar("SELECT count(*) FROM evolution_goal_test.goal_v1")
        .fetch_one(&admin)
        .await
        .unwrap();
    let new_count: i64 = sqlx::query_scalar("SELECT count(*) FROM evolution_goal_test.goal_v2")
        .fetch_one(&admin)
        .await
        .unwrap();
    assert_eq!(old_count, 0);
    assert_eq!(new_count, 4);
}
