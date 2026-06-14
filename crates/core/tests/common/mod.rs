// Each integration-test binary in `proxima-core` independently includes
// this module via `mod common;`. Items unused by a particular binary
// would otherwise trip `dead_code` even though another binary uses them.
#![allow(dead_code)]

use std::sync::Arc;

use proxima_core::personality::{InstantiatePersonalityRequest, PersonalityInstanceId};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::{
    OrgId, Owner, Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageHandle,
    UserId,
};
#[allow(unused_imports)]
pub use proxima_pg_testkit::{
    create_db, create_db_from_template, db_url, drop_db, ensure_template, unique_db_name,
};
use proxima_storage_pg::{PgStorage, core_migrator};
use sqlx::Executor;
use uuid::Uuid;

pub fn owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

pub async fn fresh_pg() -> Option<(PgStorage, String)> {
    let template = core_template_name();
    if let Err(e) = ensure_template(&template, |pool| async move {
        core_migrator().run(&pool).await.map_err(sqlx::Error::from)
    })
    .await
    {
        panic!("PG required for tests but admin connect failed: {e}");
    }

    let db_name = match create_db_from_template("proxima_core_test", &template).await {
        Ok(name) => name,
        Err(e) => {
            panic!("PG required for tests but admin connect failed: {e}");
        }
    };
    let url = db_url(&db_name);
    match PgStorage::connect(&url).await {
        Ok(pg) => Some((pg, db_name)),
        Err(err) => {
            let _ = drop_db(&db_name).await;
            panic!("PG required for tests but unavailable: {err}");
        }
    }
}

fn core_template_name() -> String {
    let mut hash = FNV_OFFSET_BASIS;
    for migration in core_migrator().iter() {
        hash = hash_bytes(hash, &migration.version.to_be_bytes());
        hash = hash_bytes(hash, migration.checksum.as_ref());
    }
    format!("proxima_tmpl_core_{hash:016x}")
}

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Apply the test sidecar table for the wake-context fixture's
/// triggering Fact. Idempotent.
pub async fn apply_wake_context_sidecars(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    pool.execute(
        "CREATE SCHEMA IF NOT EXISTS proxima_test; \
         CREATE TABLE IF NOT EXISTS proxima_test.wake_context_fact_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             label text NOT NULL \
         ); \
         CREATE TABLE IF NOT EXISTS proxima_test.wake_context_perspective_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             label text NOT NULL \
         );",
    )
    .await
    .map(|_| ())
}

/// Drop the test database when the fixture goes out of scope.
pub struct PgFixture {
    pub pg: PgStorage,
    pub db: String,
}

impl PgFixture {
    pub async fn cleanup(self) {
        drop(self.pg);
        let _ = drop_db(&self.db).await;
    }
}

/// Phase 1d Task 6 fixture: provision an owner, instantiate one
/// personality with a Root Perspective sidecar, ingest one Fact authored
/// by an external source, return the bits the wake-context assembler
/// needs.
///
/// Returns a tuple of `(storage, owner, instance_id, change_event_seq)`
/// matching the signature `assemble_wake_context` consumes.
///
/// Returns `None` if no test PG is available — callers should treat the
/// `None` arm as a skip (matches the `proxima-storage-pg` test pattern).
pub async fn seed_wake_context_fixture()
-> Option<(StorageHandle, Owner, PersonalityInstanceId, Uuid, PgFixture)> {
    let (pg, db) = fresh_pg().await?;
    pg.run_migrations().await.ok()?;
    apply_wake_context_sidecars(pg.pool()).await.ok()?;

    let owner = owner_fixture();

    // After Phase 2 Step 1, `instantiate_personality` writes the canonical
    // `proxima_core.root_personality_perspective_v1` sidecar directly.
    let response = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            display_name: "Engineer Test Personality".into(),
            purpose: "exercise wake-context assembly with a non-empty system prompt".into(),
        })
        .await
        .ok()?;

    // Ingest one external Fact authored by `external/event-source` so
    // the change event has a known seq + a triggering memory id.
    let now = time::OffsetDateTime::now_utc();
    let payload = serde_json::to_vec(&serde_json::json!({
        "label": "wake-context-test-trigger",
    }))
    .expect("payload serializes");
    let draft = EventDraft {
        source_id: SourceId::new("proxima-test/wake-context-source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        schema_id: SchemaId::new("proxima-test/wake-context-fact-v1".into()),
        schema_version: SchemaVersion::new(1),
        payload,
        observed_at: now,
        occurred_at: now,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("proxima-test/wake-context-cited-v1".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: rand_content_hash(),
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("proxima-test/wake-context-citation-v1".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    };
    let outcome = pg.ingest_event_atomic(&draft).await.ok()?;

    // The fact's typed payload lives in `wake_context_fact_v1`; the
    // event_ingest verb only writes to `proxima_core.memories` + the
    // change_event row, so we backfill the sidecar manually here.
    pg.pool()
        .execute(
            sqlx::query(
                "INSERT INTO proxima_test.wake_context_fact_v1 (memory_id, label)
                 VALUES ($1, $2)
                 ON CONFLICT (memory_id) DO UPDATE SET label = EXCLUDED.label",
            )
            .bind(outcome.memory_id.into_inner())
            .bind("wake-context-test-trigger"),
        )
        .await
        .ok()?;

    let storage: StorageHandle = Arc::new(pg.clone());
    Some((
        storage,
        owner,
        response.instance_id,
        outcome.change_event_seq,
        PgFixture { pg, db },
    ))
}

fn rand_content_hash() -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, byte) in Uuid::now_v7()
        .as_bytes()
        .iter()
        .chain(Uuid::now_v7().as_bytes().iter())
        .take(32)
        .enumerate()
    {
        out[i] = *byte;
    }
    out
}
