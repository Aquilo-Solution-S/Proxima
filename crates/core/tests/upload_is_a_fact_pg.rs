//! An upload leaves a Fact behind, and the same file leaves exactly one.
//!
//! The blob store is faked, but content-addressed exactly as the real one
//! is: it writes the same two rows `persist_completed_blob` writes, on the
//! same `(owner_kind, owner_id, schema_id, content_hash)` conflict target.
//! A permissive fake — one that minted a fresh cited object per call —
//! would let every assertion here pass under an assumption production does
//! not make, which is the whole property under test.
//!
//! Faked rather than real S3 so this runs on the default
//! `cargo test --workspace`; the real store's roundtrip is covered in
//! `proxima-blob-s3`.

use std::sync::Arc;

mod common;

use common::{ConstantEmbedding, drop_db, fresh_pg};
use proxima_core::engine::Engine;
use proxima_core::storage_ports::{
    CitedBlobPort, CitedBlobReadUrl, CitedBlobUploadAborted, CitedBlobUploadCompleted,
    CitedBlobUploadPrepared,
};
use proxima_core::{
    AuthPath, AuthzContext, FlavorRegistry, Owner, OwnerRef, StorageError, UPLOADED_BLOB_SCHEMA_ID,
    UserId,
};
use proxima_storage_pg::PgStorage;

const MODEL: &str = "test-upload-embed";

/// A blob store that has already committed its transaction — which is
/// precisely the state `complete_upload_as_fact` inherits.
struct CommittedBlobs {
    pool: sqlx::PgPool,
    /// Bytes keyed by `upload_id`, so one fake can serve several uploads
    /// and a caller can re-complete the same one.
    pending: std::collections::HashMap<String, (&'static str, &'static [u8])>,
}

impl CommittedBlobs {
    fn new(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            pending: std::collections::HashMap::new(),
        }
    }

    fn with_upload(mut self, upload_id: &str, filename: &'static str, body: &'static [u8]) -> Self {
        self.pending.insert(upload_id.to_string(), (filename, body));
        self
    }
}

#[async_trait::async_trait]
impl CitedBlobPort for CommittedBlobs {
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

    async fn complete_upload(
        &self,
        _authz: &AuthzContext,
        owner: OwnerRef,
        upload_id: &str,
    ) -> Result<CitedBlobUploadCompleted, StorageError> {
        let (filename, body) = *self
            .pending
            .get(upload_id)
            .ok_or_else(|| StorageError::ConstraintViolation(format!("no upload {upload_id}")))?;
        let blake3 = *blake3::hash(body).as_bytes();
        let (owner_kind, owner_id) = owner.columns();

        // The same upsert the real store performs: content-addressed, so
        // the same bytes under this owner resolve to the one cited object.
        let row: (uuid::Uuid, bool) = sqlx::query_as(
            "WITH ins AS (
                 INSERT INTO proxima_core.cited_objects
                     (cited_object_id, schema_id, owner_kind, owner_id, content_hash)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (owner_kind, owner_id, schema_id, content_hash) DO NOTHING
                 RETURNING cited_object_id
             )
             SELECT cited_object_id, false FROM ins
             UNION ALL
             SELECT cited_object_id, true
               FROM proxima_core.cited_objects
              WHERE owner_kind = $3
                AND owner_id IS NOT DISTINCT FROM $4
                AND schema_id = $2
                AND content_hash = $5
                AND NOT EXISTS (SELECT 1 FROM ins)
              LIMIT 1",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(UPLOADED_BLOB_SCHEMA_ID)
        .bind(owner_kind)
        .bind(owner_id)
        .bind(&blake3[..])
        .fetch_one(&self.pool)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;

        sqlx::query(
            "INSERT INTO proxima_core.cited_uploaded_blob_v1
                (cited_object_id, bucket, object_key, sha256, byte_len, mime, filename)
             VALUES ($1, 'test-bucket', $2, $3, $4, 'application/pdf', $5)
             ON CONFLICT (cited_object_id) DO NOTHING",
        )
        .bind(row.0)
        .bind(hex::encode(blake3))
        .bind(&blake3[..])
        .bind(i64::try_from(body.len()).expect("test body fits i64"))
        .bind(filename)
        .execute(&self.pool)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;

        // The store reports what it HOLDS, so a replay reports the
        // filename of the first upload, not of this call.
        let stored_filename: String = sqlx::query_scalar(
            "SELECT filename FROM proxima_core.cited_uploaded_blob_v1 WHERE cited_object_id = $1",
        )
        .bind(row.0)
        .fetch_one(&self.pool)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;

        Ok(CitedBlobUploadCompleted {
            cited_object_id: row.0.to_string(),
            schema: UPLOADED_BLOB_SCHEMA_ID.to_string(),
            content_hash: hex::encode(blake3),
            sha256: hex::encode(blake3),
            byte_len: body.len() as u64,
            mime: "application/pdf".to_string(),
            filename: stored_filename,
            idempotent_replay: row.1,
        })
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

fn engine_for(pg: &PgStorage) -> Engine {
    Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg.clone()).storage_ports())
        .with_embed(Arc::new(ConstantEmbedding::zero(MODEL)))
}

/// How many `core/upload-v1` Facts this owner holds, and what each cites.
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

/// The substrate guarantee: the artefact is stored AND its arrival is
/// recorded, joined to it by a citation, without the caller arranging
/// anything.
#[tokio::test]
async fn completing_an_upload_records_a_fact_that_cites_the_artefact() {
    let (pg, db_name) = fresh_pg().await;
    let pool = pg.pool_for_tests().clone();
    let owner: Owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let engine = engine_for(&pg);
    let blobs = CommittedBlobs::new(pool.clone()).with_upload(
        "upload-1",
        "handbuch.pdf",
        b"the bytes of a handbook",
    );

    let completed = engine
        .complete_upload_as_fact(&blobs, &authz, owner, "upload-1", &[])
        .await
        .expect("complete the upload");

    assert!(
        !completed.blob.idempotent_replay,
        "first upload of these bytes"
    );
    assert!(!completed.fact.idempotent_replay, "first record of it");

    let facts = upload_facts(&pool).await;
    assert_eq!(facts.len(), 1, "one upload, one Fact: {facts:?}");
    assert_eq!(facts[0].0, completed.fact.memory_id.into_inner());
    assert_eq!(
        facts[0].1.to_string(),
        completed.blob.cited_object_id,
        "the Fact must cite the artefact the caller was handed"
    );
    assert!(
        facts[0].2.contains("handbuch.pdf"),
        "the filename must reach the indexed text: {}",
        facts[0].2
    );

    // The Fact is searchable like any other, not a silent bookkeeping row.
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.embedding_jobs
          WHERE entity_kind = 'Fact' AND entity_id = $1 AND model_id = $2",
    )
    .bind(facts[0].0)
    .bind(MODEL)
    .fetch_one(&pool)
    .await
    .expect("count embedding jobs");
    assert_eq!(jobs, 1, "the upload Fact is queued for embedding");

    drop(pg);
    drop_db(&db_name).await.expect("drop test db");
}

/// Uploading the same bytes again is one artefact and one arrival. The
/// second call must not mint a second Fact citing the same object — the
/// corpus would then say the file arrived twice, and a reader counting
/// arrivals would be wrong.
#[tokio::test]
async fn the_same_file_uploaded_twice_is_one_artefact_and_one_upload_fact() {
    let (pg, db_name) = fresh_pg().await;
    let pool = pg.pool_for_tests().clone();
    let owner: Owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let engine = engine_for(&pg);
    // Same bytes, different names — the second caller does not get to
    // rename what the corpus already holds, so both completions describe
    // one artefact and share one receipt key.
    let blobs = CommittedBlobs::new(pool.clone())
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

    assert!(second.blob.idempotent_replay, "the bytes were already held");
    assert!(
        second.fact.idempotent_replay,
        "the arrival was already recorded"
    );
    assert_eq!(
        second.fact.memory_id, first.fact.memory_id,
        "a replay returns the Fact that already exists"
    );
    assert_eq!(second.blob.filename, "handbuch.pdf", "the stored name wins");

    let facts = upload_facts(&pool).await;
    assert_eq!(facts.len(), 1, "one file, one upload Fact: {facts:?}");

    let objects: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.cited_objects")
            .fetch_one(&pool)
            .await
            .expect("count cited objects");
    assert_eq!(objects, 1, "one file, one artefact");

    drop(pg);
    drop_db(&db_name).await.expect("drop test db");
}

/// Two distinct files are two arrivals, each citing its own artefact.
/// Without this, a receipt key that collapsed too far would pass the
/// replay test above by recording nothing at all after the first upload.
#[tokio::test]
async fn two_different_files_are_two_upload_facts() {
    let (pg, db_name) = fresh_pg().await;
    let pool = pg.pool_for_tests().clone();
    let owner: Owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let engine = engine_for(&pg);
    let blobs = CommittedBlobs::new(pool.clone())
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

    assert!(!second.blob.idempotent_replay);
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
