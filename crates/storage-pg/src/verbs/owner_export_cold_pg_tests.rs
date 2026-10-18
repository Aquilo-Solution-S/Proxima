//! Export decodes forgotten memories from their cold objects (#478).
//!
//! Crate-internal because the barrier test parks the export at a pause
//! point, and because the fault-injecting cold stores need the record's
//! digest function.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use proxima_core::owner_inverse::{
    ExportAuthorization, OwnerExportBundle, OwnerExportTarget, OwnerSurfaces,
};
use proxima_core::storage_ports::{MemoryAuthoringPort, OwnerInversePort, OwnerWritePermit};
use proxima_core::{
    AccessKind, ColdObjectStore, EntityKind, MemoryId, OwnerId, StorageError, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
use serde_json::Value;
use uuid::Uuid;

use crate::PgStorage;
use crate::test_fixtures::create_core_db;
use crate::verbs::forget::forget_pg_tests::{draft, ingest_stamped};
use crate::verbs::forget::{MemoryColdStore, cold_digest, cold_object_key};

const AGENT_NOTE: &str = "proxima_core.agent_note_v1";
const AGENT_DERIVATION: &str = "proxima_core.agent_derivation_v1";
const PASS: u8 = 0;
const TAMPER: u8 = 1;
const FAIL: u8 = 2;

/// A cold store over a shared in-memory one whose GET can be faulted, and
/// which records whether any GET ran while a transaction was open on the
/// test database.
struct FaultCold {
    inner: Arc<MemoryColdStore>,
    mode: AtomicU8,
    admin: sqlx::PgPool,
    gets_inside_a_transaction: AtomicUsize,
    gets: AtomicUsize,
}

#[async_trait::async_trait]
impl ColdObjectStore for FaultCold {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        self.inner.put(key, bytes).await
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let open: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM pg_stat_activity
              WHERE datname = current_database()
                AND pid <> pg_backend_pid()
                AND state LIKE 'idle in transaction%'",
        )
        .fetch_one(&self.admin)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
        if open > 0 {
            self.gets_inside_a_transaction
                .fetch_add(1, Ordering::SeqCst);
        }
        match self.mode.load(Ordering::SeqCst) {
            FAIL => Err(StorageError::Internal("injected cold GET failure".into())),
            TAMPER => {
                let mut bytes = self.inner.get(key).await?;
                if let Some(last) = bytes.last_mut() {
                    *last ^= 0xff;
                }
                Ok(bytes)
            }
            _ => self.inner.get(key).await,
        }
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }

    fn backend(&self) -> &str {
        self.inner.backend()
    }
}

struct Fixture {
    pg: PgStorage,
    cold: Arc<FaultCold>,
    user: UserId,
    permit: OwnerWritePermit,
    hot: Uuid,
    forgotten: Uuid,
    db_name: String,
}

fn surfaces() -> OwnerSurfaces {
    OwnerSurfaces::for_registry(&proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests())
}

/// One owner with a hot Fact and a forgotten one; the forgotten one had a
/// note sidecar and a sketch, both of which forget moved into its object.
async fn fixture() -> Fixture {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let admin = sqlx::PgPool::connect(&url).await.expect("admin pool");
    let cold = Arc::new(FaultCold {
        inner: Arc::new(MemoryColdStore::default()),
        mode: AtomicU8::new(PASS),
        admin,
        gets_inside_a_transaction: AtomicUsize::new(0),
        gets: AtomicUsize::new(0),
    });
    let pg = PgStorage::connect_for_tests(url)
        .await
        .expect("connect")
        .with_cold(cold.clone());
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let pool = pg.pool_for_tests();
    let mut hot = draft(None);
    hot.rendered_text = Some("Still hot".into());
    let hot =
        proxima_core::storage_ports::FactIngestPort::ingest_fact_atomic(&pg, &permit, &hot, None)
            .await
            .expect("hot fact")
            .memory_id
            .into_inner();
    let mut gone = draft(Some(("src", "k-cold")));
    gone.rendered_text = Some("Went cold".into());
    let written = ingest_stamped(
        pool,
        &permit,
        &gone,
        &[AGENT_NOTE.to_owned()],
        &[
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
           VALUES ($1, $1, 'cold title', 'cold body', ARRAY['tag'])",
        ],
    )
    .await
    .expect("stamped fact");
    sqlx::query(
        "INSERT INTO proxima_core.sketch (t, owner_id, kind, text)
         VALUES ($1, $2, 'fact', 'Went cold')
         ON CONFLICT (t) DO NOTHING",
    )
    .bind(written.memory_id.into_inner())
    .bind(owner.into_inner())
    .execute(pool)
    .await
    .expect("sketch");
    MemoryAuthoringPort::forget_memory(&pg, &permit, written.memory_id)
        .await
        .expect("forget");
    cold.gets.store(0, Ordering::SeqCst);
    Fixture {
        pg,
        cold,
        user,
        permit,
        hot,
        forgotten: written.memory_id.into_inner(),
        db_name,
    }
}

async fn export(fx: &Fixture) -> Result<OwnerExportBundle, StorageError> {
    let auth =
        ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id: fx.user });
    fx.pg.export_owner_bundle(&auth, &surfaces()).await
}

fn rows_for<'a>(bundle: &'a OwnerExportBundle, table: &str, t: Uuid) -> Vec<&'a Value> {
    bundle
        .table(table)
        .iter()
        .filter(|row| row["t"].as_str() == Some(t.to_string().as_str()))
        .collect()
}

fn unavailable(bundle: &OwnerExportBundle) -> Vec<(String, String)> {
    bundle
        .cold_unavailable
        .iter()
        .map(|entry| {
            (
                entry["t"].as_str().unwrap_or_default().to_owned(),
                entry["reason"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// `cooled` is append-only by trigger; a fixture that needs a row the
/// verbs would never write (a re-witnessed or pre-witness locator) bypasses
/// triggers for one statement, as an operator restoring old data would.
async fn rewrite_frozen_cooled(fx: &Fixture, digest: Option<Vec<u8>>) {
    let mut tx = fx.cold.admin.begin().await.expect("admin tx");
    sqlx::query("SET LOCAL session_replication_role = replica")
        .execute(&mut *tx)
        .await
        .expect("bypass triggers");
    sqlx::query("UPDATE proxima_core.cooled SET cold_digest = $2 WHERE t = $1")
        .bind(fx.forgotten)
        .bind(digest)
        .execute(&mut *tx)
        .await
        .expect("rewrite cooled");
    tx.commit().await.expect("commit");
}

async fn finish(fx: Fixture) {
    drop(fx.pg);
    let _ = drop_db(&fx.db_name).await;
}

/// Acceptance 1 of #478 minus bytes: the bundle carries the forgotten
/// memory's sidecar, sketch and identity beside the hot one, and the object
/// store is read only after the snapshot transaction ended.
#[tokio::test]
async fn export_decodes_a_forgotten_memory_from_its_cold_object() {
    let fx = fixture().await;
    let bundle = export(&fx).await.expect("export");
    let notes = rows_for(&bundle, AGENT_NOTE, fx.forgotten);
    assert_eq!(
        notes.len(),
        1,
        "the forgotten memory's note is decoded: {notes:?}"
    );
    assert_eq!(notes[0]["title"].as_str(), Some("cold title"));
    assert_eq!(notes[0]["body"].as_str(), Some("cold body"));
    let sketches = rows_for(&bundle, "proxima_core.sketch", fx.forgotten);
    assert_eq!(
        sketches.len(),
        1,
        "the forgotten memory's sketch is decoded"
    );
    assert_eq!(sketches[0]["text"].as_str(), Some("Went cold"));
    let cooled = rows_for(&bundle, "proxima_core.cooled", fx.forgotten);
    assert_eq!(cooled.len(), 1);
    assert_eq!(cooled[0]["schema_id"].as_str(), Some("core/test-fact-v1"));
    assert!(cooled[0].get("object_key").is_none());
    assert_eq!(rows_for(&bundle, "proxima_core.memory", fx.hot).len(), 1);
    assert!(bundle.cold_unavailable.is_empty());
    assert_eq!(bundle.count("cold_unavailable"), 0);
    assert_eq!(bundle.count(AGENT_NOTE), bundle.table(AGENT_NOTE).len());
    assert_eq!(
        bundle.count("proxima_core.sketch"),
        bundle.table("proxima_core.sketch").len()
    );
    assert_eq!(fx.cold.gets.load(Ordering::SeqCst), 1);
    assert_eq!(
        fx.cold.gets_inside_a_transaction.load(Ordering::SeqCst),
        0,
        "the export read the object store while a transaction was open"
    );
    finish(fx).await;
}

#[tokio::test]
async fn a_missing_object_is_reported_not_failed() {
    let fx = fixture().await;
    fx.cold
        .inner
        .delete(&cold_object_key(fx.forgotten))
        .await
        .expect("delete");
    let bundle = export(&fx)
        .await
        .expect("a lost object does not block the export");
    assert_eq!(
        unavailable(&bundle),
        vec![(fx.forgotten.to_string(), "missing".to_owned())]
    );
    assert_eq!(bundle.count("cold_unavailable"), 1);
    assert_eq!(
        rows_for(&bundle, "proxima_core.cooled", fx.forgotten).len(),
        1
    );
    assert!(rows_for(&bundle, AGENT_NOTE, fx.forgotten).is_empty());
    finish(fx).await;
}

#[tokio::test]
async fn a_tampered_object_is_reported_invalid() {
    let fx = fixture().await;
    fx.cold.mode.store(TAMPER, Ordering::SeqCst);
    let bundle = export(&fx).await.expect("export");
    assert_eq!(
        unavailable(&bundle),
        vec![(fx.forgotten.to_string(), "invalid".to_owned())]
    );
    finish(fx).await;
}

#[tokio::test]
async fn an_unknown_layout_is_reported_with_its_version() {
    let fx = fixture().await;
    let key = cold_object_key(fx.forgotten);
    let mut bytes = fx.cold.inner.get(&key).await.expect("object");
    bytes[0] = 9;
    fx.cold.inner.put(&key, &bytes).await.expect("rewrite");
    rewrite_frozen_cooled(&fx, Some(cold_digest(&bytes))).await;
    let bundle = export(&fx).await.expect("export");
    assert_eq!(
        unavailable(&bundle),
        vec![(fx.forgotten.to_string(), "unsupported_format(9)".to_owned())]
    );
    finish(fx).await;
}

#[tokio::test]
async fn a_pre_witness_row_is_reported() {
    let fx = fixture().await;
    rewrite_frozen_cooled(&fx, None).await;
    let bundle = export(&fx).await.expect("export");
    assert_eq!(
        unavailable(&bundle),
        vec![(fx.forgotten.to_string(), "pre_witness".to_owned())]
    );
    finish(fx).await;
}

#[tokio::test]
async fn a_missing_object_with_a_queued_purge_is_reported_purge_pending() {
    let fx = fixture().await;
    let key = cold_object_key(fx.forgotten);
    fx.cold.inner.delete(&key).await.expect("delete");
    sqlx::query(
        "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id) VALUES ($1, $2)",
    )
    .bind(&key)
    .bind(fx.permit.owner().into_inner())
    .execute(fx.pg.pool_for_tests())
    .await
    .expect("queue purge");
    let bundle = export(&fx).await.expect("export");
    assert_eq!(
        unavailable(&bundle),
        vec![(fx.forgotten.to_string(), "purge_pending".to_owned())]
    );
    finish(fx).await;
}

#[tokio::test]
async fn a_store_error_fails_the_export() {
    let fx = fixture().await;
    fx.cold.mode.store(FAIL, Ordering::SeqCst);
    let err = export(&fx)
        .await
        .expect_err("a transient store error is not a verdict");
    assert!(
        matches!(err, StorageError::Internal(ref msg) if msg.contains("injected")),
        "{err:?}"
    );
    finish(fx).await;
}

/// One agent-derivation memory (Abstraction or Perspective) made from
/// `origin`, through the typed write so `sidecar_tables` is stamped and the
/// owner's content row is ensured.
async fn derive(fx: &Fixture, kind: &str, origin: (EntityKind, Uuid), body: &str) -> Uuid {
    use proxima_core::verbs::fact_ingest::AuthorizedFactWrite;
    use proxima_core::{AbstractionPayload, AgentDerivationV1, SchemaId, SidecarPayload};
    let mut cmd = draft(None);
    cmd.kind = kind.into();
    cmd.schema_id = SchemaId::new(AgentDerivationV1::SCHEMA_ID.into());
    cmd.lexical_language =
        Some(proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned());
    cmd.additional_references = vec![proxima_core::EdgeEndpoint::memory(
        origin.0,
        MemoryId::new(origin.1),
    )];
    let payload = AgentDerivationV1 {
        title: format!("{kind} title"),
        body: body.into(),
        tags: Vec::new(),
        idempotency_key: None,
        source_memory_ids: vec![origin.1],
        model_id: "test".into(),
        client_name: "test".into(),
        client_version: "1".into(),
    };
    let payload = if kind == "perspective" {
        SidecarPayload::perspective(payload)
    } else {
        SidecarPayload::abstraction(payload)
    };
    let authorized = AuthorizedFactWrite::new_for_tests(
        OwnerWritePermit::new_for_tests(*fx.permit.owner(), AccessKind::Fact),
        cmd,
        Some(AgentDerivationV1::sidecar_table().to_owned()),
        Vec::new(),
    )
    .with_sidecar_payloads_for_tests(vec![payload]);
    proxima_core::storage_ports::FactIngestPort::ingest_fact_with_typed_sidecar(
        &fx.pg,
        &authorized,
        &[],
    )
    .await
    .expect("derivation")
    .memory_id
    .into_inner()
}

/// The content row an A/P memory (hot or cooled) names.
async fn content_of(fx: &Fixture, t: Uuid) -> Uuid {
    sqlx::query_scalar(
        "SELECT content_id FROM proxima_core.memory WHERE t = $1
         UNION ALL
         SELECT content_id FROM proxima_core.cooled WHERE t = $1",
    )
    .bind(t)
    .fetch_one(&fx.cold.admin)
    .await
    .expect("content id")
}

/// Round-2 review of #478: a forgotten Abstraction and Perspective carry
/// their `agent_derivation_v1` bodies in their cold objects, and the export
/// decodes them beside a hot one; each one's content row is in the bundle.
#[tokio::test]
async fn export_decodes_forgotten_abstraction_and_perspective_bodies() {
    let fx = fixture().await;
    let fact = (EntityKind::Fact, fx.hot);
    let hot_a = derive(&fx, "abstraction", fact, "hot abstraction body").await;
    let cold_a = derive(&fx, "abstraction", fact, "cold abstraction body").await;
    // A Perspective is made from an Abstraction.
    let cold_p = derive(
        &fx,
        "perspective",
        (EntityKind::Abstraction, hot_a),
        "cold perspective body",
    )
    .await;
    for (t, kind) in [
        (cold_a, AccessKind::Abstraction),
        (cold_p, AccessKind::Perspective),
    ] {
        let permit = OwnerWritePermit::new_for_tests(*fx.permit.owner(), kind);
        MemoryAuthoringPort::forget_memory(&fx.pg, &permit, MemoryId::new(t))
            .await
            .expect("forget derivation");
    }
    let bundle = export(&fx).await.expect("export");
    assert!(
        bundle.cold_unavailable.is_empty(),
        "{:?}",
        bundle.cold_unavailable
    );
    for (t, body) in [
        (hot_a, "hot abstraction body"),
        (cold_a, "cold abstraction body"),
        (cold_p, "cold perspective body"),
    ] {
        let rows = rows_for(&bundle, AGENT_DERIVATION, t);
        assert_eq!(rows.len(), 1, "the derivation body of {t} is exported");
        assert_eq!(rows[0]["body"].as_str(), Some(body));
        let content = content_of(&fx, t).await.to_string();
        assert!(
            bundle
                .table("proxima_core.content")
                .iter()
                .any(|row| row["content_id"].as_str() == Some(content.as_str())),
            "the content row of {t} is exported"
        );
    }
    for t in [cold_a, cold_p] {
        assert_eq!(rows_for(&bundle, "proxima_core.cooled", t).len(), 1);
        assert!(rows_for(&bundle, "proxima_core.memory", t).is_empty());
    }
    assert_eq!(
        bundle.count(AGENT_DERIVATION),
        bundle.table(AGENT_DERIVATION).len()
    );
    finish(fx).await;
}
