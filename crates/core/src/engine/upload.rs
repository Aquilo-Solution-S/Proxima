//! Completing an upload: the artefact and the record of its arrival, in
//! one write.
//!
//! [`Engine::complete_upload_as_fact`] and its expectation-bearing sibling
//! are the two entries to one completion verb. [`CitedBlobPort`] stages bytes
//! and records the transfer locator and SHA-256 audit digest while returning
//! the BLAKE3 content address, but it writes no corpus
//! rows, so a caller reaching past the Engine would leave the corpus with
//! nothing in it.
//!
//! The shape follows from where the transaction has to be. Persisting a
//! cited object is a Fact write with an inline citation — storage already
//! upserts the object on `(owner, schema, content_hash)` and inserts its
//! typed row through the registered cited-object sidecar — so once the
//! Fact carries the artefact as its citation, the two cannot come apart.
//! What could not be in that transaction is the object-store work:
//! streaming, hashing, and copying an S3 object is not a database
//! statement and must not hold a transaction open while it runs. Hence the
//! split: [`CitedBlobPort::stage_upload`] before, one transaction, then
//! [`CitedBlobPort::finish_upload`] after.
//!
//! The blob port arrives as an argument rather than as an `Engine` field.
//! The store is host-wired and optional (a host without S3 has none), while
//! the `Engine` is not; making it a field would push that optionality into
//! every construction site to serve one verb.

use uuid::Uuid;

use super::Engine;
use super::errors::map_write_storage_error;
use crate::access::Relation;
use crate::authz::EngineAuthority;
use crate::citations::{
    UPLOADED_BLOB_SCHEMA_ID, UPLOADED_BLOB_WHOLE_SCHEMA_ID, UploadedBlobPayload,
    UploadedBlobWholeV1,
};
use crate::error::ProtocolError;
use crate::storage_ports::{CitedBlobService, CitedBlobStaged, CitedBlobUploadCompleted};
use crate::verbs::fact_ingest::{
    FactWriteCommand, InlineCitationMappingDraft, InlineCitedObjectDraft,
};
use crate::{
    CitationMappingPayload, CitedObjectPayload, FactIngestOutcome, OwnerRef, SchemaId,
    SchemaVersion, SidecarPayload, SourceBatchId, UploadV1,
};

/// Provenance of every upload Fact. Constant, not per-call: the source is
/// the upload lane itself, and the identity of the individual file lives in
/// the receipt key (see [`UploadV1::receipt_key`]). A per-call source id
/// would make every completion a distinct receipt and defeat replay.
const UPLOAD_SOURCE_ID: &str = "core/upload";

/// Immutable metadata a caller may use to validate the bytes staged for one
/// upload before core authorizes or persists the upload Fact.
///
/// The fields stay private so the frozen values cannot be mutated after
/// construction. The type carries no storage locator, upload status, or
/// completion claim; it is deliberately not serializable because this is an
/// in-process assertion to compare, not a backend witness or wire argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadCompletionExpectation {
    content_hash: [u8; 32],
    byte_len: u64,
    mime: String,
    filename: String,
}

impl UploadCompletionExpectation {
    /// Freeze the metadata the caller computed before uploading the bytes.
    /// MIME and filename use the same surrounding-whitespace
    /// canonicalization as upload preparation; case and interior whitespace
    /// remain part of the immutable expectation.
    #[must_use]
    pub fn new(
        content_hash: [u8; 32],
        byte_len: u64,
        mime: impl Into<String>,
        filename: impl Into<String>,
    ) -> Self {
        Self {
            content_hash,
            byte_len,
            mime: mime.into().trim().to_owned(),
            filename: filename.into().trim().to_owned(),
        }
    }

    #[must_use]
    pub const fn content_hash(&self) -> &[u8; 32] {
        &self.content_hash
    }

    #[must_use]
    pub const fn byte_len(&self) -> u64 {
        self.byte_len
    }

    #[must_use]
    pub fn mime(&self) -> &str {
        &self.mime
    }

    #[must_use]
    pub fn filename(&self) -> &str {
        &self.filename
    }
}

/// A finished upload: the stored artefact, and the Fact that records it.
/// Both were written together, so neither exists without the other.
#[derive(Debug, Clone)]
pub struct UploadCompleted {
    /// The artefact as the corpus now holds it.
    pub blob: CitedBlobUploadCompleted,
    /// The `core/upload-v1` Fact citing that artefact.
    pub fact: FactIngestOutcome,
}

impl Engine {
    /// Complete a pending upload and record it as a `core/upload-v1` Fact
    /// citing the stored artefact.
    ///
    /// `extensions` are extra typed sidecar rows a flavor wants against
    /// this Fact — extra columns on an event the substrate defines. They
    /// land in the Fact's own transaction or not at all; pass `&[]` for
    /// none. A flavor may only ADD rows against schemas it has registered,
    /// and cannot alter the Fact itself.
    ///
    /// # What is atomic
    ///
    /// The cited object, its typed `cited_uploaded_blob_v1` row, the
    /// citation, the Fact, its receipt, its embedding job, and every
    /// extension row are one transaction. Nothing a reader can observe
    /// survives a failure partway: an artefact with no recorded arrival is
    /// not a state this verb can leave behind.
    ///
    /// Two things sit outside it, both deliberately, and neither is corpus
    /// content. Staging performs one bounded object read, records the
    /// transfer's canonical locator and SHA-256 audit digest, carries the
    /// BLAKE3 content address forward, and best-effort retires the redundant
    /// pending copy — no transaction may be held open across that. Finishing
    /// marks the upload row and retries the same cleanup. A crash before
    /// finishing leaves an upload row still saying `pending` whose artefact
    /// is already recorded; completing the same upload again resolves it.
    ///
    /// # Idempotency
    ///
    /// Safe to call again with the same `upload_id`. Staging derives its
    /// canonical key from that `upload_id`, so a repeat verifies and reads
    /// exactly the same immutable object; the Fact replays on its receipt and
    /// returns the same `memory_id` and cited object, and finishing tolerates
    /// an upload already completed against the same artefact.
    ///
    /// # Errors
    ///
    /// Returns the blob store's failures mapped like any write (a missing,
    /// expired, aborted, or length-mismatched upload is `InvalidArgument`);
    /// `Forbidden` when `authz` resolves no single writable owner;
    /// `InvalidArgument` when the Fact write is rejected; and `Internal`
    /// when the resolved write owner is not the owner the artefact was
    /// stored under.
    pub async fn complete_upload_as_fact<A>(
        &self,
        blobs: &CitedBlobService,
        authority: &A,
        owner: OwnerRef,
        upload_id: &str,
        extensions: &[SidecarPayload],
    ) -> Result<UploadCompleted, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        self.complete_upload_as_fact_inner(blobs, authority, owner, upload_id, extensions, None)
            .await
    }

    /// Complete a pending upload after checking caller-supplied immutable
    /// metadata against the bytes staged by the blob service.
    ///
    /// The expectation-bearing path stages exactly once. A mismatch returns
    /// before citation authorization or persistence; staging records the
    /// canonical locator and SHA-256 audit digest, carries the BLAKE3 content
    /// address forward but writes no corpus rows, and retires
    /// the pending transfer copy. A corrected expectation can retry against
    /// those canonical bytes. Replacing the bytes requires abort plus a new
    /// prepare.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` naming the first mismatched metadata field,
    /// or the same completion errors as [`Self::complete_upload_as_fact`].
    pub async fn complete_upload_as_fact_with_expectation<A>(
        &self,
        blobs: &CitedBlobService,
        authority: &A,
        owner: OwnerRef,
        upload_id: &str,
        extensions: &[SidecarPayload],
        expectation: &UploadCompletionExpectation,
    ) -> Result<UploadCompleted, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        self.complete_upload_as_fact_inner(
            blobs,
            authority,
            owner,
            upload_id,
            extensions,
            Some(expectation),
        )
        .await
    }

    async fn complete_upload_as_fact_inner<A>(
        &self,
        blobs: &CitedBlobService,
        authority: &A,
        owner: OwnerRef,
        upload_id: &str,
        extensions: &[SidecarPayload],
        expectation: Option<&UploadCompletionExpectation>,
    ) -> Result<UploadCompleted, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let _operation = self.operation_authority(authority)?;
        // Staging does the object-store half and stops: bytes verified,
        // moved to the canonical key derived from `upload_id`; no corpus rows
        // are written here. Everything below this line is one transaction.
        let staged = blobs
            .stage_upload(authority, owner, upload_id)
            .await
            .map_err(|err| map_write_storage_error(err, "upload_id", "upload not found"))?;

        if let Some(expectation) = expectation {
            validate_staged_payload(expectation, &staged.payload)?;
        }

        self.persist_staged_upload_as_fact(blobs, authority, owner, upload_id, extensions, staged)
            .await
    }

    async fn persist_staged_upload_as_fact<A>(
        &self,
        blobs: &CitedBlobService,
        authority: &A,
        owner: OwnerRef,
        upload_id: &str,
        extensions: &[SidecarPayload],
        staged: CitedBlobStaged,
    ) -> Result<UploadCompleted, ProtocolError>
    where
        A: EngineAuthority + ?Sized,
    {
        let content_hash = hex::encode(staged.payload.content_hash);
        let payload = UploadV1 {
            filename: staged.payload.filename.clone(),
            mime: staged.payload.mime.clone(),
            byte_len: staged.payload.byte_len,
            content_hash: content_hash.clone(),
        };
        // No lexical language is stamped: a filename is not prose, and
        // guessing a text-search configuration from one would be a guess
        // about the document, which nobody has read yet.
        let draft = FactWriteCommand::from_payload(
            UPLOAD_SOURCE_ID,
            SourceBatchId::new(Uuid::now_v7()),
            &payload,
            time::OffsetDateTime::now_utc(),
        );

        // Inline, carrying the artefact's full typed payload. Storage
        // upserts the cited object on `(owner, schema, content_hash)` —
        // the same key the blob lane deduplicates on — inserts its
        // `cited_uploaded_blob_v1` row through the registered cited-object
        // sidecar, and writes the citation and the Fact, all in the one
        // transaction. That is the point of the split: the artefact and
        // the record of its arrival are now the same write.
        let cited_object = InlineCitedObjectDraft {
            schema_id: SchemaId::new(UPLOADED_BLOB_SCHEMA_ID.to_string()),
            schema_version: SchemaVersion::new(UploadedBlobPayload::SCHEMA_VERSION),
            payload_bytes: serde_json::to_vec(&staged.payload).map_err(|err| {
                ProtocolError::internal(format!("serialize staged blob payload: {err}"))
            })?,
        };
        let authorized = self
            .authorize_fact_with_citation(
                authority,
                Relation::Editor,
                draft,
                cited_object,
                whole_blob_mapping(),
                extensions,
            )
            .await?;

        let resolved = *authorized.owner_write_permit().owner();
        if resolved != owner {
            return Err(ProtocolError::internal(format!(
                "upload was staged under {owner:?} but the Fact write resolved to {resolved:?}; \
                 refusing to record an upload against an owner that does not hold the artefact",
            )));
        }

        let embedding_client = self.embed_client();
        let embedding_model_id = embedding_client.as_ref().map(|client| client.model_id());
        let fact = self
            .ingest_fact_with_citation_and_typed_sidecar(
                &authorized,
                extensions,
                embedding_model_id,
            )
            .await?;

        // Present on every citation-bearing write, including a replay,
        // where it is read back from the Fact that already exists.
        let cited_object_id = fact.cited_object_id.ok_or_else(|| {
            ProtocolError::internal(
                "upload Fact was written with a citation but storage reported no cited object",
            )
        })?;

        // Outside the transaction, and necessarily so: it marks the
        // upload row and deletes the now-redundant pending object, one of
        // which is not a database write at all. A crash before this leaves
        // an upload row still saying `pending` while its artefact and the
        // Fact are committed — bookkeeping for the transfer protocol,
        // invisible in the corpus, and repaired by completing again.
        blobs
            .finish_upload(authority, owner, upload_id, cited_object_id)
            .await
            .map_err(|err| map_write_storage_error(err, "upload_id", "upload not found"))?;

        let blob = CitedBlobUploadCompleted {
            cited_object_id: cited_object_id.to_string(),
            schema: UPLOADED_BLOB_SCHEMA_ID.to_string(),
            content_hash,
            sha256: hex::encode(staged.payload.sha256),
            byte_len: staged.payload.byte_len,
            mime: staged.payload.mime,
            filename: staged.payload.filename,
            // "You added nothing new." True when this owner's upload of
            // these bytes was already recorded, and when the upload id
            // itself had already been completed.
            idempotent_replay: fact.idempotent_replay || staged.already_completed.is_some(),
        };
        Ok(UploadCompleted { blob, fact })
    }
}

fn validate_staged_payload(
    expectation: &UploadCompletionExpectation,
    staged: &crate::citations::UploadedBlobPayload,
) -> Result<(), ProtocolError> {
    if staged.content_hash != *expectation.content_hash() {
        return Err(ProtocolError::invalid_argument(
            "content_hash",
            "staged upload does not match expected BLAKE3 content hash",
        ));
    }
    if staged.byte_len != expectation.byte_len() {
        return Err(ProtocolError::invalid_argument(
            "byte_len",
            "staged upload does not match expected byte length",
        ));
    }
    if staged.mime != expectation.mime() {
        return Err(ProtocolError::invalid_argument(
            "mime",
            "staged upload does not match expected MIME",
        ));
    }
    if staged.filename != expectation.filename() {
        return Err(ProtocolError::invalid_argument(
            "filename",
            "staged upload does not match expected filename",
        ));
    }
    Ok(())
}

/// "This Fact came from that artefact", with no locator — the mapping is
/// the whole citation. `UploadedBlobWholeV1` is a braced empty struct, so
/// its wire form is `{}`; it is serialized rather than hardcoded so the
/// two cannot drift.
fn whole_blob_mapping() -> InlineCitationMappingDraft {
    InlineCitationMappingDraft {
        schema_id: SchemaId::new(UPLOADED_BLOB_WHOLE_SCHEMA_ID.to_string()),
        schema_version: SchemaVersion::new(UploadedBlobWholeV1::SCHEMA_VERSION),
        payload_bytes: serde_json::to_vec(&UploadedBlobWholeV1 {})
            .expect("UploadedBlobWholeV1 is an empty struct and always serializes"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;

    use super::*;
    use crate::storage::StorageError;
    use crate::storage_ports::{
        CitedBlobHeld, CitedBlobPort, CitedBlobReadUrl, CitedBlobUploadAborted,
        CitedBlobUploadPrepared,
    };
    use crate::{AuthPath, AuthzContext, FlavorRegistry, UserId};

    /// The mapping this verb sends must be the one core registered for
    /// whole-artefact citations, and must target the uploaded-blob
    /// cited-object schema — otherwise `authorize_fact_with_citation_by_ref`
    /// would refuse it, at runtime, on every upload.
    #[test]
    fn the_mapping_targets_the_uploaded_blob_schema() {
        let mapping = whole_blob_mapping();

        assert_eq!(mapping.schema_id.as_str(), UPLOADED_BLOB_WHOLE_SCHEMA_ID);
        assert_eq!(
            UploadedBlobWholeV1::cited_object_schema().as_str(),
            crate::citations::UPLOADED_BLOB_SCHEMA_ID
        );
    }

    /// The typed ingest boundary requires every payload to be a JSON
    /// object; `{}` is what an empty mapping must look like on the wire.
    #[test]
    fn the_mapping_payload_is_a_json_object() {
        assert_eq!(whole_blob_mapping().payload_bytes, b"{}");
    }

    fn staged_payload() -> crate::citations::UploadedBlobPayload {
        crate::citations::UploadedBlobPayload {
            content_hash: [0x11; 32],
            bucket: "private-bucket".to_owned(),
            object_key: "objects/upload-id".to_owned(),
            sha256: [0x22; 32],
            byte_len: 7,
            mime: "application/pdf".to_owned(),
            filename: "book.pdf".to_owned(),
            etag: Some("etag".to_owned()),
            uploaded_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn exact_staged_payload_satisfies_expectation() {
        let payload = staged_payload();
        let expectation = UploadCompletionExpectation::new(
            payload.content_hash,
            payload.byte_len,
            payload.mime.clone(),
            payload.filename.clone(),
        );

        validate_staged_payload(&expectation, &payload)
            .expect("the exact staged payload must satisfy its expectation");
    }

    #[test]
    fn expectation_trims_only_surrounding_text_whitespace() {
        let expectation = UploadCompletionExpectation::new(
            [0x11; 32],
            7,
            "  Application/PDF ; charset=utf-8  ",
            "  my  book.pdf  ",
        );

        assert_eq!(expectation.mime(), "Application/PDF ; charset=utf-8");
        assert_eq!(expectation.filename(), "my  book.pdf");
    }

    #[derive(Debug)]
    struct CountingStagePort {
        staged: CitedBlobStaged,
        stage_calls: Arc<AtomicUsize>,
        finish_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl CitedBlobPort for CountingStagePort {
        async fn prepare_upload(
            &self,
            _authz: &crate::AuthzContext,
            _owner: crate::OwnerRef,
            _filename: &str,
            _mime: &str,
            _byte_len: u64,
        ) -> Result<CitedBlobUploadPrepared, StorageError> {
            Err(StorageError::Internal("test port: unused".to_owned()))
        }

        async fn stage_upload(
            &self,
            _authz: &crate::AuthzContext,
            _owner: crate::OwnerRef,
            _upload_id: &str,
        ) -> Result<CitedBlobStaged, StorageError> {
            self.stage_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.staged.clone())
        }

        async fn finish_upload(
            &self,
            _authz: &crate::AuthzContext,
            _owner: crate::OwnerRef,
            _upload_id: &str,
            _cited_object_id: uuid::Uuid,
        ) -> Result<(), StorageError> {
            self.finish_calls.fetch_add(1, Ordering::SeqCst);
            Err(StorageError::Internal("test port: unused".to_owned()))
        }

        async fn abort_upload(
            &self,
            _authz: &crate::AuthzContext,
            _owner: crate::OwnerRef,
            _upload_id: &str,
        ) -> Result<CitedBlobUploadAborted, StorageError> {
            Err(StorageError::Internal("test port: unused".to_owned()))
        }

        async fn read_url(
            &self,
            _authz: &crate::AuthzContext,
            _owner: crate::OwnerRef,
            _cited_object_id: uuid::Uuid,
        ) -> Result<CitedBlobReadUrl, StorageError> {
            Err(StorageError::Internal("test port: unused".to_owned()))
        }

        async fn find_held_blobs(
            &self,
            _authz: &crate::AuthzContext,
            _owner: crate::OwnerRef,
            _content_hashes: &[[u8; 32]],
        ) -> Result<Vec<CitedBlobHeld>, StorageError> {
            Err(StorageError::Internal("test port: unused".to_owned()))
        }
    }

    async fn assert_engine_rejects_mismatch(
        expectation: UploadCompletionExpectation,
        expected_message: &str,
    ) {
        let stage_calls = Arc::new(AtomicUsize::new(0));
        let finish_calls = Arc::new(AtomicUsize::new(0));
        let service = CitedBlobService::new(Arc::new(CountingStagePort {
            staged: CitedBlobStaged {
                payload: staged_payload(),
                already_completed: None,
            },
            stage_calls: Arc::clone(&stage_calls),
            finish_calls: Arc::clone(&finish_calls),
        }));
        let owner = crate::OwnerRef::Personal(UserId::new(uuid::Uuid::nil()));
        let authority = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());

        let error = engine
            .complete_upload_as_fact_with_expectation(
                &service,
                &authority,
                owner,
                "upload-id",
                &[],
                &expectation,
            )
            .await
            .expect_err("metadata mismatch must stop before the rejecting storage port");

        assert_eq!(error.code, crate::ErrorCode::InvalidArgument);
        assert_eq!(error.message, expected_message);
        assert_eq!(stage_calls.load(Ordering::SeqCst), 1);
        assert_eq!(finish_calls.load(Ordering::SeqCst), 0);
        assert!(!error.message.contains("private-bucket"));
        assert!(!error.message.contains("objects/upload-id"));
    }

    #[tokio::test]
    async fn engine_rejects_content_hash_mismatch_before_authorization() {
        assert_engine_rejects_mismatch(
            UploadCompletionExpectation::new([0x33; 32], 7, "application/pdf", "book.pdf"),
            "invalid argument content_hash: staged upload does not match expected BLAKE3 content hash",
        )
        .await;
    }

    #[tokio::test]
    async fn engine_rejects_byte_len_mismatch_before_authorization() {
        assert_engine_rejects_mismatch(
            UploadCompletionExpectation::new([0x11; 32], 8, "application/pdf", "book.pdf"),
            "invalid argument byte_len: staged upload does not match expected byte length",
        )
        .await;
    }

    #[tokio::test]
    async fn engine_rejects_mime_mismatch_before_authorization() {
        assert_engine_rejects_mismatch(
            UploadCompletionExpectation::new([0x11; 32], 7, "text/plain", "book.pdf"),
            "invalid argument mime: staged upload does not match expected MIME",
        )
        .await;
    }

    #[tokio::test]
    async fn engine_rejects_filename_mismatch_before_authorization() {
        assert_engine_rejects_mismatch(
            UploadCompletionExpectation::new([0x11; 32], 7, "application/pdf", "other.pdf"),
            "invalid argument filename: staged upload does not match expected filename",
        )
        .await;
    }

    #[tokio::test]
    async fn engine_reports_hash_before_length() {
        assert_engine_rejects_mismatch(
            UploadCompletionExpectation::new([0x33; 32], 8, "application/pdf", "book.pdf"),
            "invalid argument content_hash: staged upload does not match expected BLAKE3 content hash",
        )
        .await;
    }

    #[tokio::test]
    async fn engine_reports_length_before_mime() {
        assert_engine_rejects_mismatch(
            UploadCompletionExpectation::new([0x11; 32], 8, "text/plain", "book.pdf"),
            "invalid argument byte_len: staged upload does not match expected byte length",
        )
        .await;
    }

    #[tokio::test]
    async fn engine_reports_mime_before_filename() {
        assert_engine_rejects_mismatch(
            UploadCompletionExpectation::new([0x11; 32], 7, "text/plain", "other.pdf"),
            "invalid argument mime: staged upload does not match expected MIME",
        )
        .await;
    }
}
