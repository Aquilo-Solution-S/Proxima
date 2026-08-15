//! A flavor extends a Fact the substrate owns.
//!
//! One Fact, two sidecar rows: the substrate's own, and a flavor's extra
//! columns against the same `memory_id`. This is the mechanism that replaces
//! a post-persist hook, and these tests are the reason it can be trusted --
//! the widening from `&SidecarPayload` to `&[SidecarPayload]` compiles
//! whether or not the extra payloads are ever written, so nothing but a test
//! distinguishes "extends" from "silently drops".
//!
//! Three properties, and the second is the load-bearing one:
//!
//!   1. every payload in the slice lands
//!   2. a bad payload rolls back the Fact AND the good sidecar with it
//!   3. destination is chosen by the payload's own schema, not by position
//!
//! (2) is what makes a hook unnecessary. A hook was proposed so a flavor
//! could write inside core's transaction; passing data achieves the same
//! atomicity while leaving the transaction handle -- which is total
//! authority over core's own rows -- entirely inside storage.

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::storage_ports::FactIngestPort;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AuthPath, AuthzContext, Engine, FactPayload, FlavorRegistry, FlavorRegistryFrozen, MemoryId,
    Owner, PayloadKeyBuilder, Relation, SidecarPayload, SourceBatchId,
};
use proxima_storage_pg::sidecars::{PgMemoryPayload, PgMemoryPayloadFuture, PgSidecarReadCtx};
use proxima_storage_pg::verbs::fact_ingest::{FactIngestSidecarFuture, PgFactSidecar};
use proxima_storage_pg::{PgSidecarRegistry, PgSidecarRegistryFrozen, register_core_pg_sidecars};
use sqlx::{Postgres, Transaction};
use std::sync::Arc;
use uuid::Uuid;

// --------------------------------------------------------------- the Fact
// What the substrate owns. In the motivating case this is "a file entered
// the corpus"; here it is deliberately anonymous, because the mechanism must
// not care which event is being extended.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SubstrateEventV1 {
    note: String,
}

impl FactPayload for SubstrateEventV1 {
    const SCHEMA_ID: &'static str = "test/substrate-event-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("note", &self.note);
        key.finish()
    }

    fn render(&self) -> String {
        self.note.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.substrate_event_sidecar")
    }
}

impl PgFactSidecar for SubstrateEventV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> FactIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.substrate_event_sidecar (memory_id, note) VALUES ($1, $2)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.note)
            .execute(tx.as_mut())
            .await
            .map_err(|e| proxima_core::StorageError::Internal(e.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for SubstrateEventV1 {
    fn load_memory_payload(
        _ctx: PgSidecarReadCtx<'_>,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move { Ok(None) })
    }
}

// ---------------------------------------------------------- the extension
// What a FLAVOR adds. A separate registered schema with its own table; it is
// never the Fact's own schema, only ever an extra payload in the slice.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct FlavorExtensionV1 {
    flavor_field: String,
}

impl FactPayload for FlavorExtensionV1 {
    const SCHEMA_ID: &'static str = "test/flavor-extension-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("flavor_field", &self.flavor_field);
        key.finish()
    }

    fn render(&self) -> String {
        self.flavor_field.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.flavor_extension_sidecar")
    }
}

impl PgFactSidecar for FlavorExtensionV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> FactIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.flavor_extension_sidecar (memory_id, flavor_field)
                 VALUES ($1, $2)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.flavor_field)
            .execute(tx.as_mut())
            .await
            .map_err(|e| proxima_core::StorageError::Internal(e.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for FlavorExtensionV1 {
    fn load_memory_payload(
        _ctx: PgSidecarReadCtx<'_>,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move { Ok(None) })
    }
}

/// Schema + PG sidecar registered, table never created.
/// `freeze_against` already refuses a schema with a sidecar table and no
/// PG sidecar, so the reachable failure is migration-not-run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct UnmigratedExtensionV1 {
    whatever: String,
}

impl FactPayload for UnmigratedExtensionV1 {
    const SCHEMA_ID: &'static str = "test/unmigrated-extension-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("whatever", &self.whatever);
        key.finish()
    }

    fn render(&self) -> String {
        self.whatever.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.unmigrated_extension_sidecar")
    }
}

impl PgFactSidecar for UnmigratedExtensionV1 {
    fn insert_sidecar<'t>(
        self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> FactIngestSidecarFuture<'t>
    where
        Self: 't,
    {
        Box::pin(async move {
            // public.unmigrated_extension_sidecar is deliberately never created.
            sqlx::query(
                "INSERT INTO public.unmigrated_extension_sidecar (memory_id, whatever)
                 VALUES ($1, $2)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.whatever)
            .execute(tx.as_mut())
            .await
            .map_err(|e| proxima_core::StorageError::Internal(e.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for UnmigratedExtensionV1 {
    fn load_memory_payload(
        _ctx: PgSidecarReadCtx<'_>,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move { Ok(None) })
    }
}

// ------------------------------------------------------------- scaffolding

fn registry() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema_or_panic_for_tests::<SubstrateEventV1>();
    registry.add_fact_schema_or_panic_for_tests::<FlavorExtensionV1>();
    registry.add_fact_schema_or_panic_for_tests::<UnmigratedExtensionV1>();
    registry.freeze_or_panic_for_tests()
}

fn pg_sidecars() -> PgSidecarRegistryFrozen {
    let registry = registry();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_fact::<SubstrateEventV1>();
    sidecars.add_fact::<FlavorExtensionV1>();
    sidecars.add_fact::<UnmigratedExtensionV1>();
    sidecars
        .freeze_against(registry.schemas())
        .expect("extension test sidecars match schemas")
}

async fn create_sidecar_tables(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE public.substrate_event_sidecar (
            memory_id uuid PRIMARY KEY,
            note text NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE public.flavor_extension_sidecar (
            memory_id uuid PRIMARY KEY,
            flavor_field text NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

fn draft(note: &str) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    let payload = SubstrateEventV1 {
        note: note.to_string(),
    };
    FactWriteCommand::from_payload(
        "test/extension-source",
        SourceBatchId::new(Uuid::now_v7()),
        &payload,
        now,
    )
}

fn authz_for(owner: Owner) -> AuthzContext {
    AuthzContext::single_owner(&owner, AuthPath::HostBearer)
}

async fn substrate_rows(pool: &sqlx::PgPool, memory_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM public.substrate_event_sidecar WHERE memory_id = $1")
        .bind(memory_id)
        .fetch_one(pool)
        .await
        .expect("count query")
}

async fn extension_rows(pool: &sqlx::PgPool, memory_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM public.flavor_extension_sidecar WHERE memory_id = $1")
        .bind(memory_id)
        .fetch_one(pool)
        .await
        .expect("count query")
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn a_flavor_extension_lands_alongside_the_substrate_sidecar() {
    let (pg, db_name) = fresh_pg().await;
    let pg = pg.with_sidecars(pg_sidecars());
    create_sidecar_tables(pg.pool_for_tests())
        .await
        .expect("sidecar tables");

    let owner = owner_fixture();
    let engine = Engine::new(registry()).with_storage_ports(Arc::new(pg.clone()).storage_ports());
    let authz = authz_for(owner);

    let authorized = engine
        .authorize_fact_ingest(&authz, Relation::Ingest, draft("both rows"), &[])
        .await
        .expect("authorized");

    let outcome = pg
        .ingest_fact_with_typed_sidecar(
            &authorized,
            &[
                SidecarPayload::fact(SubstrateEventV1 {
                    note: "both rows".into(),
                }),
                SidecarPayload::fact(FlavorExtensionV1 {
                    flavor_field: "the flavor's own column".into(),
                }),
            ],
            None,
        )
        .await
        .expect("a Fact with one extension ingests");

    let memory_id = outcome.memory_id.into_inner();
    assert_eq!(
        substrate_rows(pg.pool_for_tests(), memory_id).await,
        1,
        "the substrate's own sidecar row is missing"
    );
    assert_eq!(
        extension_rows(pg.pool_for_tests(), memory_id).await,
        1,
        "the flavor extension was accepted by the signature and silently dropped — \
         which is exactly what this test exists to catch"
    );

    drop_db(&db_name).await.expect("drop test db");
}

#[tokio::test]
async fn a_failed_extension_rolls_back_the_fact_and_the_good_sidecar() {
    let (pg, db_name) = fresh_pg().await;
    let pg = pg.with_sidecars(pg_sidecars());
    create_sidecar_tables(pg.pool_for_tests())
        .await
        .expect("sidecar tables");

    let owner = owner_fixture();
    let engine = Engine::new(registry()).with_storage_ports(Arc::new(pg.clone()).storage_ports());
    let authz = authz_for(owner);

    let authorized = engine
        .authorize_fact_ingest(&authz, Relation::Ingest, draft("must not survive"), &[])
        .await
        .expect("authorized");

    // Good payload FIRST, unroutable one second: the failure therefore
    // happens after a sidecar row has already been inserted, which is the
    // only ordering that can prove rollback rather than mere refusal.
    let err = pg
        .ingest_fact_with_typed_sidecar(
            &authorized,
            &[
                SidecarPayload::fact(SubstrateEventV1 {
                    note: "must not survive".into(),
                }),
                SidecarPayload::fact(UnmigratedExtensionV1 {
                    whatever: "this table was never created".into(),
                }),
            ],
            None,
        )
        .await
        .expect_err("an extension whose table is missing must refuse the whole write");

    assert!(
        format!("{err:?}").contains("unmigrated_extension_sidecar"),
        "expected the failure to name the missing relation, got {err:?}"
    );

    let facts: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.memories WHERE schema_id = $1")
            .bind(SubstrateEventV1::SCHEMA_ID)
            .fetch_one(pg.pool_for_tests())
            .await
            .expect("count facts");
    assert_eq!(
        facts, 0,
        "the Fact survived a failed extension — the write was not atomic"
    );

    let orphans: i64 = sqlx::query_scalar("SELECT count(*) FROM public.substrate_event_sidecar")
        .fetch_one(pg.pool_for_tests())
        .await
        .expect("count sidecars");
    assert_eq!(
        orphans, 0,
        "the substrate sidecar row survived a failed extension: a flavor's mistake \
         left the substrate's own table dirty"
    );

    drop_db(&db_name).await.expect("drop test db");
}
