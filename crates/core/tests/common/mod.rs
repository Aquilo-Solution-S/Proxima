// Each integration-test binary in `proxima-core` independently includes
// this module via `mod common;`. Items unused by a particular binary
// would otherwise trip `dead_code` even though another binary uses them.
#![allow(dead_code)]

use std::sync::Arc;

use proxima_core::personality::{
    InstantiatePersonalityRequest, PersonalityInstanceId, PersonalitySelfDraft,
};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    OrgId, Owner, Principal, SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageHandle,
    UserId,
};
use proxima_storage_pg::PgStorage;
use sqlx::{Connection, Executor, PgConnection};
use uuid::Uuid;

/// Override via `PROXIMA_TEST_PG_URL` (e.g. the `docker-compose.dev.yml`
/// PG: `postgres://proxima:proxima@localhost/proxima`). The default
/// targets a peer-auth local PG with a `postgres` superuser, matching
/// the `proxima-storage-pg` test harness.
const DEFAULT_ADMIN_URL: &str = "postgres://postgres@localhost/postgres";

pub fn admin_url() -> String {
    std::env::var("PROXIMA_TEST_PG_URL").unwrap_or_else(|_| DEFAULT_ADMIN_URL.into())
}

pub fn db_url(name: &str) -> String {
    let admin = admin_url();
    match admin.rfind('/') {
        Some(idx) => format!("{}/{}", &admin[..idx], name),
        None => format!("{admin}/{name}"),
    }
}

pub fn owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::nil())),
        org_id: OrgId::new(Uuid::nil()),
    }
}

pub async fn fresh_pg() -> Option<(PgStorage, String)> {
    let db_name = format!("proxima_core_test_{}", Uuid::now_v7().simple());
    if create_db(&db_name).await.is_err() {
        eprintln!("skipping (no admin PG)");
        return None;
    }
    let url = db_url(&db_name);
    match PgStorage::connect(&url).await {
        Ok(pg) => Some((pg, db_name)),
        Err(err) => {
            let _ = drop_db(&db_name).await;
            eprintln!("skipping (PG unavailable): {err}");
            None
        }
    }
}

pub async fn create_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(&admin_url()).await?;
    conn.execute(format!("CREATE DATABASE \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

pub async fn drop_db(name: &str) -> Result<(), sqlx::Error> {
    let mut conn = PgConnection::connect(&admin_url()).await?;
    conn.execute(format!("DROP DATABASE IF EXISTS \"{name}\"").as_str())
        .await?;
    conn.close().await?;
    Ok(())
}

/// Apply the test sidecar table for the wake-context fixture's
/// triggering Fact. Idempotent.
pub async fn apply_wake_context_sidecars(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    pool.execute(
        "CREATE SCHEMA IF NOT EXISTS proxima_test; \
         CREATE TABLE IF NOT EXISTS proxima_test.wake_context_self_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             display_name text NOT NULL, \
             purpose text NOT NULL \
         ); \
         CREATE TABLE IF NOT EXISTS proxima_test.wake_context_fact_v1 ( \
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

    // Instantiate one personality with the test self-sidecar populated
    // with a non-empty purpose so `system_prompt` ends up non-empty.
    let self_draft = PersonalitySelfDraft {
        schema_id: SchemaId::new("proxima-test/wake-context-self-v1".into()),
        schema_version: SchemaVersion::new(1),
        text: "Engineer Test Personality".into(),
        typed_payload: serde_json::json!({
            "display_name": "Engineer Test Personality",
            "purpose": "exercise wake-context assembly with a non-empty system prompt",
        }),
    };
    let response = pg
        .instantiate_personality(
            &InstantiatePersonalityRequest {
                owner: owner.clone(),
                personality_type_id: "proxima-test/wake-context-personality-v1".into(),
                payload_overrides: None,
            },
            &self_draft,
            "proxima_test.wake_context_self_v1",
        )
        .await
        .ok()?;

    // Backfill the `root_personality_perspective_v1` sidecar — the
    // PG verb that stores the typed self payload writes to whatever
    // sidecar table we supply; the wake-context assembler reads from
    // the canonical `root_personality_perspective_v1` shape.
    pg.pool()
        .execute(
            sqlx::query(
                "INSERT INTO proxima_core.root_personality_perspective_v1
                    (memory_id, display_name, purpose)
                 SELECT p.current_root_perspective_memory_id,
                        s.display_name,
                        s.purpose
                 FROM proxima_core.personality p
                 JOIN proxima_test.wake_context_self_v1 s
                   ON s.memory_id = p.current_root_perspective_memory_id
                 WHERE p.personality_instance_id = $1
                 ON CONFLICT (memory_id) DO UPDATE
                    SET display_name = EXCLUDED.display_name,
                        purpose = EXCLUDED.purpose",
            )
            .bind(response.instance_id.into_inner()),
        )
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
        owner: owner.clone(),
        schema_id: SchemaId::new("proxima-test/wake-context-fact-v1".into()),
        schema_version: SchemaVersion::new(1),
        payload,
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("proxima-test/wake-context-cited-v1".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: rand_content_hash(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("proxima-test/wake-context-citation-v1".into()),
            schema_version: SchemaVersion::new(1),
        },
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
