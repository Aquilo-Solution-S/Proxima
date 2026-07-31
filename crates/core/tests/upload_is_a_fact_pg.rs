//! An upload is one write: the artefact and the record of its arrival.
//!
//! The blob store is faked down to what it now actually does — verify and
//! stage bytes, then be told the id they were recorded under. It no longer
//! needs to imitate a content-addressed upsert, because the upsert is not
//! its job any more; the substrate performs it, which is what these tests
//! check.
//!
//! Faked rather than real S3 so this runs on the default
//! `cargo test --workspace`; the real store's roundtrip is covered in
//! `proxima-blob-s3`.

use std::sync::{Arc, Mutex};

mod common;

use common::{ConstantEmbedding, drop_db, fresh_pg};
use proxima_core::citations::UploadedBlobPayload;
use proxima_core::engine::Engine;
use proxima_core::storage_ports::{
    CitedBlobPort, CitedBlobReadUrl, CitedBlobStaged, CitedBlobUploadAborted,
    CitedBlobUploadPrepared,
};
use proxima_core::{
    AuthPath, AuthzContext, FactPayload, FlavorRegistry, Owner, OwnerRef, PayloadKeyBuilder,
    SidecarPayload, StorageError, UserId,
};
use proxima_storage_pg::PgStorage;

const MODEL: &str = "test-upload-embed";

/// A blob store that has staged its bytes and recorded nothing.
struct StagingBlobs {
    /// Bytes keyed by `upload_id`.
    pending: std::collections::HashMap<String, (&'static str, &'static [u8])>,
    /// Every `finish_upload` this store was told to perform. Empty means
    /// the caller never got far enough to record anything.
    finished: Mutex<Vec<(String, uuid::Uuid)>>,
}

impl StagingBlobs {
    fn new() -> Self {
        Self {
            pending: std::collections::HashMap::new(),
            finished: Mutex::new(Vec::new()),
        }
    }

    fn with_upload(mut self, upload_id: &str, filename: &'static str, body: &'static [u8]) -> Self {
        self.pending.insert(upload_id.to_string(), (filename, body));
        self
    }

    fn finished(&self) -> Vec<(String, uuid::Uuid)> {
        self.finished.lock().expect("lock").clone()
    }
}

#[async_trait::async_trait]
impl CitedBlobPort for StagingBlobs {
    async fn prepare_upload(
        &self,
        _authz: &AuthzContext,
        _owner: OwnerRef,
        _filename: &str,
        _mime: &str,
        _byte_len: u64,
    ) -> Result<CitedBlobUploadPrepared, StorageError> {
        unimplemented!("the fixture prepares its uploads up front")
    }

    async fn stage_upload(
        &self,
        _authz: &AuthzContext,
        _owner: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobStaged, StorageError> {
        let (filename, body) = *self
            .pending
            .get(upload_id)
            .ok_or_else(|| StorageError::ConstraintViolation(format!("no upload {upload_id}")))?;
        let content_hash = *blake3::hash(body).as_bytes();
        Ok(CitedBlobStaged {
            payload: UploadedBlobPayload {
                content_hash,
                bucket: "test-bucket".into(),
                object_key: format!("objects/test/{}", hex::encode(content_hash)),
                sha256: content_hash,
                byte_len: body.len() as u64,
                mime: "application/pdf".into(),
                filename: filename.into(),
                etag: None,
                uploaded_at: time::OffsetDateTime::UNIX_EPOCH,
            },
            already_completed: None,
        })
    }

    async fn finish_upload(
        &self,
        _authz: &AuthzContext,
        _owner: OwnerRef,
        upload_id: &str,
        cited_object_id: uuid::Uuid,
    ) -> Result<(), StorageError> {
        self.finished
            .lock()
            .expect("lock")
            .push((upload_id.to_string(), cited_object_id));
        Ok(())
    }

    async fn abort_upload(
        &self,
        _authz: &AuthzContext,
        _owner: OwnerRef,
        _upload_id: &str,
    ) -> Result<CitedBlobUploadAborted, StorageError> {
        unimplemented!("not exercised")
    }

    async fn read_url(
        &self,
        _authz: &AuthzContext,
        _owner: OwnerRef,
        _cited_object_id: uuid::Uuid,
    ) -> Result<CitedBlobReadUrl, StorageError> {
        unimplemented!("not exercised")
    }
}

/// A Fact payload that no PG sidecar routes. It stands in for a flavor
/// extension whose migration never ran — the failure a flavor can actually
/// reach, since a schema declaring a sidecar table with no PG sidecar
/// registered is refused at boot instead.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct UnroutableExtensionV1 {
    note: String,
}

impl FactPayload for UnroutableExtensionV1 {
    const SCHEMA_ID: &'static str = "test/unroutable-extension-v1";
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
        Some("public.unroutable_extension")
    }
}

fn engine_for(pg: &PgStorage) -> Engine {
    Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports())
        .with_embed(Arc::new(ConstantEmbedding::zero(MODEL)))
}

fn owner_and_authz() -> (Owner, AuthzContext) {
    let owner: Owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    (owner, authz)
}

/// Every `core/upload-v1` Fact, with the object it cites and its text.
async fn upload_facts(pool: &sqlx::PgPool) -> Vec<(uuid::Uuid, uuid::Uuid, String)> {
    sqlx::query_as(
        "SELECT m.memory_id, cm.cited_object_id, m.text
           FROM proxima_core.memories m
           JOIN proxima_core.citation_mappings cm USING (citation_mapping_id)
          WHERE m.schema_id = 'core/upload-v1'
          ORDER BY m.memory_id",
    )
    .fetch_all(pool)
    .await
    .expect("read upload facts")
}

// One literal per counter rather than a helper taking the statement: a
// helper that accepts `sql` is a dynamic-SQL site to the policy checker
// even when every caller passes a literal, and the ratchet is there to
// keep that number falling.
async fn cited_object_count(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cited_objects")
        .fetch_one(pool)
        .await
        .expect("count cited objects")
}

async fn stored_blob_count(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cited_uploaded_blob_v1")
        .fetch_one(pool)
        .await
        .expect("count stored blobs")
}

async fn fact_embedding_job_count(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.embedding_jobs WHERE entity_kind = 'Fact'",
    )
    .fetch_one(pool)
    .await
    .expect("count embedding jobs")
}

/// The artefact and the record of its arrival are one write, and the blob
/// store is told the id only after that write committed.
#[tokio::test]
async fn completing_an_upload_records_the_artefact_and_its_arrival_together() {
    let (pg, db_name) = fresh_pg().await;
    let pool = pg.pool_for_tests().clone();
    let (owner, authz) = owner_and_authz();
    let engine = engine_for(&pg);
    let blobs =
        StagingBlobs::new().with_upload("upload-1", "handbuch.pdf", b"the bytes of a handbook");

    let completed = engine
        .complete_upload_as_fact(&blobs, &authz, owner, "upload-1", &[])
        .await
        .expect("complete the upload");

    assert!(!completed.blob.idempotent_replay);
    assert!(!completed.fact.idempotent_replay);

    // The cited object and its typed row: written by the substrate through
    // the registered cited-object sidecar, not by the blob store.
    let blob_row: (String, String, String, i64) = sqlx::query_as(
        "SELECT b.bucket, b.object_key, b.filename, b.byte_len
           FROM proxima_core.cited_uploaded_blob_v1 b
           JOIN proxima_core.cited_objects co USING (cited_object_id)
          WHERE co.schema_id = 'core/uploaded-blob-v1'",
    )
    .fetch_one(&pool)
    .await
    .expect("the staged artefact is stored");
    assert_eq!(blob_row.0, "test-bucket");
    assert_eq!(blob_row.2, "handbuch.pdf");
    assert_eq!(blob_row.3, 23);

    let facts = upload_facts(&pool).await;
    assert_eq!(facts.len(), 1, "one upload, one Fact: {facts:?}");
    assert_eq!(facts[0].0, completed.fact.memory_id.into_inner());
    assert_eq!(
        facts[0].1.to_string(),
        completed.blob.cited_object_id,
        "the Fact cites the artefact the caller was handed"
    );
    assert!(facts[0].2.contains("handbuch.pdf"), "{}", facts[0].2);

    // The blob store learns the id only from the committed write.
    assert_eq!(
        blobs.finished(),
        vec![("upload-1".to_string(), facts[0].1)],
        "the upload is closed out against the object that was recorded"
    );

    // READABLE, FINDABLE, NOT EMBEDDED. `UploadV1::EMBEDDABLE` is false,
    // and the assertion above that the text carries `handbuch.pdf` is the
    // other half of the same statement: the filename still reaches
    // `memories.text`, so full-text search can find the file, and only the
    // vector is declined.
    assert_eq!(
        fact_embedding_job_count(&pool).await,
        0,
        "an upload Fact was queued for embedding; its render is a template \
         with a filename in it and every upload produces a near-identical \
         one, which is a crowded index rather than a useful neighbourhood"
    );

    drop(pg);
    drop_db(&db_name).await.expect("drop test db");
}

/// Reconciliation must not undo the write path's decision.
///
/// The load-bearing half of `EMBEDDABLE`, and the half a write-path-only
/// implementation gets wrong. Reconcile heals the *absence* of a job —
/// exactly the state a non-embeddable schema is supposed to stay in — so
/// without the same exclusion it would find every row the write path
/// skipped and queue it on the next operator pass. The bug would not
/// appear until someone ran maintenance.
#[tokio::test]
async fn reconciliation_does_not_queue_what_the_write_path_declined() {
    let (pg, db_name) = fresh_pg().await;
    let pool = pg.pool_for_tests().clone();
    let (owner, authz) = owner_and_authz();
    let engine = engine_for(&pg);
    let blobs =
        StagingBlobs::new().with_upload("upload-1", "handbuch.pdf", b"the bytes of a handbook");

    engine
        .complete_upload_as_fact(&blobs, &authz, owner, "upload-1", &[])
        .await
        .expect("completion");
    assert_eq!(fact_embedding_job_count(&pool).await, 0);

    // A Fact with text and no vector is precisely what reconcile exists to
    // find, so this call is the adversary, not a formality.
    let outcome = engine
        .reconcile_embeddings(proxima_core::EmbeddingReconcileScope::MissingOnly, None)
        .await
        .expect("reconcile");

    assert_eq!(
        fact_embedding_job_count(&pool).await,
        0,
        "reconciliation queued an upload Fact the write path deliberately \
         skipped: {outcome:?}"
    );
    assert_eq!(
        outcome.enqueued, 0,
        "reconcile reported work it should not have found"
    );

    drop(pg);
    drop_db(&db_name).await.expect("drop test db");
}

/// The exclusion is a declaration, not a hardcoded list of schema ids.
///
/// Without this, a `non_embeddable_schema_ids()` that returned a constant
/// would pass every other test in this file. It also pins the default:
/// a schema that says nothing keeps its vector, because a silently
/// missing embedding reports no error anywhere.
#[test]
fn the_registry_reads_embeddability_from_the_schema() {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();

    assert!(
        !registry.schema_is_embeddable(proxima_core::UploadV1::SCHEMA_ID),
        "core/upload-v1 declares EMBEDDABLE = false"
    );
    assert!(
        registry.schema_is_embeddable(proxima_core::UtteranceV1::SCHEMA_ID),
        "a schema that declares nothing keeps its vector"
    );
    assert!(
        registry.schema_is_embeddable("nobody/registered-this-v1"),
        "an unknown schema is embeddable: opting out has to be said out loud, \
         and a missing vector is silent where a surplus one is merely waste"
    );
    assert_eq!(
        registry.non_embeddable_schema_ids(),
        [proxima_core::UploadV1::SCHEMA_ID],
        "the list is derived from the declarations, not written down twice"
    );
}

/// **The point of the change.** When the Fact write is refused, the
/// artefact must not survive it. While completion was two transactions the
/// cited object was already committed by the time anything could refuse,
/// and a failure here left a file in the corpus whose arrival nothing
/// recorded.
#[tokio::test]
async fn a_refused_fact_write_leaves_no_artefact_behind() {
    let (pg, db_name) = fresh_pg().await;
    let pool = pg.pool_for_tests().clone();
    let (owner, authz) = owner_and_authz();
    let engine = engine_for(&pg);
    let blobs =
        StagingBlobs::new().with_upload("upload-1", "handbuch.pdf", b"the bytes of a handbook");

    let err = engine
        .complete_upload_as_fact(
            &blobs,
            &authz,
            owner,
            "upload-1",
            &[SidecarPayload::fact(UnroutableExtensionV1 {
                note: "a flavor row with nowhere to land".into(),
            })],
        )
        .await
        .expect_err("an unroutable extension must refuse the write");
    assert!(
        err.to_string().contains("test/unroutable-extension-v1"),
        "the refusal names the schema that could not be routed: {err}"
    );

    assert_eq!(
        cited_object_count(&pool).await,
        0,
        "the artefact rolled back with the Fact"
    );
    assert_eq!(
        stored_blob_count(&pool).await,
        0,
        "and so did its typed row"
    );
    assert!(upload_facts(&pool).await.is_empty(), "and the Fact itself");
    assert!(
        blobs.finished().is_empty(),
        "an upload that recorded nothing is never closed out"
    );

    drop(pg);
    drop_db(&db_name).await.expect("drop test db");
}

/// Uploading the same bytes again is one artefact and one arrival. The
/// second call must not mint a second Fact citing the same object — the
/// corpus would then say the file arrived twice.
#[tokio::test]
async fn the_same_file_uploaded_twice_is_one_artefact_and_one_upload_fact() {
    let (pg, db_name) = fresh_pg().await;
    let pool = pg.pool_for_tests().clone();
    let (owner, authz) = owner_and_authz();
    let engine = engine_for(&pg);
    // Same bytes, different names. The second completion replays the Fact
    // on its content hash, so the name it staged never reaches the corpus.
    let blobs = StagingBlobs::new()
        .with_upload("upload-1", "handbuch.pdf", b"the bytes of a handbook")
        .with_upload("upload-2", "kopie.pdf", b"the bytes of a handbook");

    let first = engine
        .complete_upload_as_fact(&blobs, &authz, owner, "upload-1", &[])
        .await
        .expect("first completion");
    let second = engine
        .complete_upload_as_fact(&blobs, &authz, owner, "upload-2", &[])
        .await
        .expect("second completion");

    assert!(second.fact.idempotent_replay, "the arrival was recorded");
    assert!(second.blob.idempotent_replay);
    assert_eq!(
        second.fact.memory_id, first.fact.memory_id,
        "a replay returns the Fact that already exists"
    );
    assert_eq!(
        second.blob.cited_object_id, first.blob.cited_object_id,
        "and the object it cites, read back from that Fact's mapping"
    );

    assert_eq!(upload_facts(&pool).await.len(), 1, "one file, one Fact");
    assert_eq!(cited_object_count(&pool).await, 1, "one file, one artefact");
    let stored_filename: String =
        sqlx::query_scalar("SELECT filename FROM proxima_core.cited_uploaded_blob_v1")
            .fetch_one(&pool)
            .await
            .expect("read filename");
    assert_eq!(
        stored_filename, "handbuch.pdf",
        "the corpus keeps the name it recorded first"
    );
    // The response names what THIS call staged, and the two deliberately
    // disagree on a replay (docs/11 §Findable, not embedded). Pinned from
    // both sides: the assertion above reads the stored row, this one reads
    // the response, and a change that collapsed them would otherwise be
    // invisible — which is exactly how the stored-row property lost its
    // carrier once before, when it was asserted on the response struct.
    assert_eq!(
        second.blob.filename, "kopie.pdf",
        "the caller is answered with the name it just uploaded, not one it never sent"
    );
    assert_eq!(
        first.blob.filename, "handbuch.pdf",
        "and the first call is unaffected"
    );

    drop(pg);
    drop_db(&db_name).await.expect("drop test db");
}

/// Two distinct files are two arrivals citing their own artefacts. Without
/// this, a receipt key that collapsed too far would pass the replay test
/// above by recording nothing at all after the first upload.
#[tokio::test]
async fn two_different_files_are_two_upload_facts() {
    let (pg, db_name) = fresh_pg().await;
    let pool = pg.pool_for_tests().clone();
    let (owner, authz) = owner_and_authz();
    let engine = engine_for(&pg);
    let blobs = StagingBlobs::new()
        .with_upload("upload-1", "handbuch.pdf", b"the bytes of a handbook")
        .with_upload("upload-2", "atlas.pdf", b"the bytes of an atlas");

    let first = engine
        .complete_upload_as_fact(&blobs, &authz, owner, "upload-1", &[])
        .await
        .expect("first completion");
    let second = engine
        .complete_upload_as_fact(&blobs, &authz, owner, "upload-2", &[])
        .await
        .expect("second completion");

    assert!(!second.fact.idempotent_replay);
    assert_ne!(first.fact.memory_id, second.fact.memory_id);

    let facts = upload_facts(&pool).await;
    assert_eq!(facts.len(), 2, "two files, two Facts: {facts:?}");
    assert_ne!(
        facts[0].1, facts[1].1,
        "each Fact cites its own artefact, not a shared one"
    );

    drop(pg);
    drop_db(&db_name).await.expect("drop test db");
}
